package web

import (
	"context"
	"embed"
	"errors"
	"fmt"
	"html/template"
	"io/fs"
	"log"
	"log/slog"
	"net"
	"net/http"
	"strings"
	"sync"
	"time"

	"github.com/go-webauthn/webauthn/webauthn"
	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/live"
	"github.com/hugoh/tamis/internal/store"
)

var errNoAssets = errors.New("this build has no web interface files")

const (
	sessionCookie  = "tamis_session"
	ceremonyCookie = "tamis_ceremony"
	sessionTTL     = 12 * time.Hour
	ceremonyTTL    = 5 * time.Minute

	contentSecurityPolicy = "default-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'"
)

//go:embed static templates
var assets embed.FS

// Server is the web interface. There is one user, who holds the passkeys.
type Server struct {
	db         *store.Store
	log        *slog.Logger
	wa         *webauthn.WebAuthn
	origin     string
	secure     bool
	pages      map[string]*template.Template
	cfg        *config.Config
	files      fs.FS
	version    string
	run        RunActions
	status     *live.Status
	logs       *live.Ring
	actionMu   sync.Mutex
	mux        *http.ServeMux
	sessions   table[struct{}]
	ceremonies table[ceremony]
}

// Options are the web interface's connections to the running daemon. Any may
// be left out: the pages that need it say so.
type Options struct {
	// Run connects to the mailbox for undo and learn.
	Run RunActions
	// Status is what the daemon is doing.
	Status *live.Status
	// Logs holds its recent log lines.
	Logs *live.Ring
	// Version is shown at the foot of every page; empty shows nothing.
	Version string
}

// New builds the web interface for an enabled configuration.
func New(
	cfg *config.Config,
	database *store.Store,
	logger *slog.Logger,
	opts Options,
) (*Server, error) {
	return newServer(cfg, database, logger, opts, assets)
}

// HasAssets reports whether this binary has the web interface's front-end
// files. They are not committed: the build supplies them (mise run
// vendor-web), and a build without them still works, just without the web
// interface.
func HasAssets() bool {
	return len(missingAssets(assets)) == 0
}

// requiredAssets are the generated files the pages load.
func requiredAssets() []string {
	return []string{"static/vendor/theme.css", "static/vendor/htmx.min.js"}
}

func missingAssets(fsys fs.FS) []string {
	var missing []string

	for _, name := range requiredAssets() {
		if _, err := fs.Stat(fsys, name); err != nil {
			missing = append(missing, name)
		}
	}

	return missing
}

func newServer(
	cfg *config.Config,
	database *store.Store,
	logger *slog.Logger,
	opts Options,
	fsys fs.FS,
) (*Server, error) {
	if missing := missingAssets(fsys); len(missing) > 0 {
		return nil, fmt.Errorf("%w (%s): run mise run vendor-web, then rebuild",
			errNoAssets, strings.Join(missing, ", "))
	}

	origin := strings.TrimSuffix(cfg.Web.Origin, "/")

	relyingParty, err := webauthn.New(&webauthn.Config{
		RPDisplayName: ownerName,
		RPID:          cfg.Web.RPID(),
		RPOrigins:     []string{origin},
	})
	if err != nil {
		return nil, fmt.Errorf("set up passkeys: %w", err)
	}

	pages, err := parsePages(fsys)
	if err != nil {
		return nil, err
	}

	srv := &Server{
		db:      database,
		log:     logger,
		wa:      relyingParty,
		origin:  origin,
		secure:  strings.HasPrefix(origin, "https://"),
		pages:   pages,
		cfg:     cfg,
		files:   fsys,
		version: opts.Version,
		run:     opts.Run,
		status:  opts.Status,
		logs:    opts.Logs,
		mux:     http.NewServeMux(),
	}
	srv.routes()

	return srv, nil
}

// ServeHTTP implements http.Handler.
func (s *Server) ServeHTTP(resp http.ResponseWriter, req *http.Request) {
	header := resp.Header()
	header.Set("Content-Security-Policy", contentSecurityPolicy)
	header.Set("X-Content-Type-Options", "nosniff")
	header.Set("Referrer-Policy", "no-referrer")
	header.Set("Cache-Control", "no-store")

	if req.Method != http.MethodGet && req.Method != http.MethodHead &&
		req.Header.Get("Origin") != s.origin {
		http.Error(resp, "forbidden", http.StatusForbidden)

		return
	}

	s.mux.ServeHTTP(resp, req)
}

// parsePages gives every page its own template set: the layout plus the
// page's own "content" block.
func parsePages(fsys fs.FS) (map[string]*template.Template, error) {
	pages := map[string]*template.Template{}

	for _, name := range []string{"login", "enroll", pageDecisions, pageHealth, pageWhitelist, pageLogs} {
		page, err := template.ParseFS(fsys, "templates/layout.html", "templates/"+name+".html")
		if err != nil {
			return nil, fmt.Errorf("parse %s page: %w", name, err)
		}

		pages[name] = page
	}

	return pages, nil
}

const (
	readHeaderTimeout = 10 * time.Second
	shutdownTimeout   = 5 * time.Second
)

// Serve answers requests on listener until ctx ends, then shuts down
// gracefully. A requested shutdown is not an error.
func (s *Server) Serve(ctx context.Context, listener net.Listener) error {
	httpServer := &http.Server{
		Handler:           s,
		ReadHeaderTimeout: readHeaderTimeout,
		ErrorLog:          log.New(serverErrors{s.log}, "", 0),
	}
	failed := make(chan error, 1)

	go func() { failed <- httpServer.Serve(listener) }()

	select {
	case err := <-failed:
		return fmt.Errorf("serve: %w", err)
	case <-ctx.Done():
	}

	shutdownCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), shutdownTimeout)
	defer cancel()

	if err := httpServer.Shutdown(shutdownCtx); err != nil {
		return fmt.Errorf("shut down: %w", err)
	}

	return nil
}

func (s *Server) routes() {
	s.mux.Handle("GET /static/", http.FileServerFS(s.files))
	s.mux.HandleFunc("GET /auth/login", s.bare("login", "sign in"))
	s.mux.HandleFunc("GET /auth/enroll", s.bare("enroll", "enroll a passkey"))
	s.mux.HandleFunc("POST /auth/register/begin", s.registerBegin)
	s.mux.HandleFunc("POST /auth/register/finish", s.registerFinish)
	s.mux.HandleFunc("POST /auth/login/begin", s.loginBegin)
	s.mux.HandleFunc("POST /auth/login/finish", s.loginFinish)
	s.mux.HandleFunc("POST /auth/logout", s.logout)
	s.mux.Handle("GET /{$}", s.requireSession(http.HandlerFunc(s.decisions)))
	s.mux.Handle("GET /health", s.requireSession(http.HandlerFunc(s.health)))
	s.mux.Handle("GET /logs", s.requireSession(http.HandlerFunc(s.logLines)))
	s.mux.Handle("GET /whitelist", s.requireSession(http.HandlerFunc(s.whitelist)))
	s.mux.Handle("POST /whitelist", s.requireSession(http.HandlerFunc(s.whitelistAdd)))
	s.mux.Handle("POST /whitelist/remove", s.requireSession(http.HandlerFunc(s.whitelistRemove)))
	s.mux.Handle(
		"POST /whitelist/remove/confirm",
		s.requireSession(http.HandlerFunc(s.whitelistConfirm)),
	)
	s.mux.Handle(
		"GET /whitelist/remove/cancel",
		s.requireSession(http.HandlerFunc(s.whitelistCancel)),
	)
	s.mux.Handle("POST /decisions/{id}/undo", s.requireSession(http.HandlerFunc(s.undo)))
	s.mux.Handle("POST /decisions/{id}/learn", s.requireSession(http.HandlerFunc(s.learn)))
}

// bare serves a page that has no navigation: the ones before you sign in.
func (s *Server) bare(name, title string) http.HandlerFunc {
	return func(resp http.ResponseWriter, _ *http.Request) {
		s.render(resp, name, view{Title: title})
	}
}

// serverErrors carries net/http's own error log into tamis's. A client that
// connects and leaves before speaking TLS (a port check, a browser that
// doesn't trust the certificate) is routine, so those lines are debug level.
type serverErrors struct{ logger *slog.Logger }

func (e serverErrors) Write(line []byte) (int, error) {
	message := strings.TrimSpace(string(line))

	level := slog.LevelWarn
	if strings.Contains(message, "TLS handshake error") {
		level = slog.LevelDebug
	}

	e.logger.Log(context.Background(), level, "web server", "detail", message)

	return len(line), nil
}
