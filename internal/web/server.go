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
	debug      *live.DebugSwitch
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
	// Debug lets the Logs page turn debug logging on for a while; without it
	// the page has no such switch.
	Debug *live.DebugSwitch
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
		debug:   opts.Debug,
		mux:     http.NewServeMux(),
	}
	srv.routes()

	return srv, nil
}

// ServeHTTP implements http.Handler.
func (s *Server) ServeHTTP(writer http.ResponseWriter, req *http.Request) {
	resp := &statusWriter{ResponseWriter: writer, status: http.StatusOK}
	started := time.Now()

	ctx := req.Context()

	defer func() {
		s.log.DebugContext(ctx, "web request",
			"method", req.Method, "path", req.URL.Path,
			"status", resp.status, "duration", time.Since(started))
	}()

	header := resp.Header()
	header.Set("Content-Security-Policy", contentSecurityPolicy)
	header.Set("X-Content-Type-Options", "nosniff")
	header.Set("Referrer-Policy", "no-referrer")
	header.Set("Cache-Control", "no-store")

	if req.Method != http.MethodGet && req.Method != http.MethodHead && !s.fromOwnOrigin(req) {
		http.Error(resp, "forbidden", http.StatusForbidden)

		return
	}

	s.mux.ServeHTTP(resp, req)
}

// statusWriter remembers the status code for the access log.
type statusWriter struct {
	http.ResponseWriter

	status int
}

func (w *statusWriter) WriteHeader(code int) {
	w.status = code
	w.ResponseWriter.WriteHeader(code)
}

// Unwrap lets http.ResponseController reach the real writer.
func (w *statusWriter) Unwrap() http.ResponseWriter { return w.ResponseWriter }

// parsePages gives every page its own template set: the layout plus the
// page's own "content" block.
func parsePages(fsys fs.FS) (map[string]*template.Template, error) {
	pages := map[string]*template.Template{}

	for _, name := range []string{"login", "enroll", pageDecisions, pageHealth, pageWhitelist, pageRules, pageLogs} {
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
	readTimeout       = 30 * time.Second
	idleTimeout       = 2 * time.Minute
	shutdownTimeout   = 5 * time.Second
)

// Serve answers requests on listener until ctx ends, then shuts down
// gracefully. A requested shutdown is not an error.
func (s *Server) Serve(ctx context.Context, listener net.Listener) error {
	httpServer := &http.Server{
		Handler:           s,
		ReadHeaderTimeout: readHeaderTimeout,
		ReadTimeout:       readTimeout,
		IdleTimeout:       idleTimeout,
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

// fromOwnOrigin accepts the configured Origin, or a same-origin request whose
// Origin the browser blanked to "null" because of Referrer-Policy: no-referrer.
// Sec-Fetch-Site is set by the browser and cannot be forged by a page.
func (s *Server) fromOwnOrigin(req *http.Request) bool {
	switch req.Header.Get("Origin") {
	case s.origin:
		return true
	case "null":
		return req.Header.Get("Sec-Fetch-Site") == "same-origin"
	default:
		return false
	}
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
	s.authed("GET /{$}", s.decisions)
	s.authed("GET /health", s.health)
	s.authed("GET /rules", s.rules)
	s.authed("POST /rules/test", s.rulesTest)
	s.authed("GET /logs", s.logLines)
	s.authed("POST /logs/debug", s.setDebug)
	s.authed("GET /whitelist", s.whitelist)
	s.authed("POST /whitelist", s.whitelistAdd)
	s.authed("POST /whitelist/remove", s.whitelistRemove)
	s.authed("POST /whitelist/remove/confirm", s.whitelistConfirm)
	s.authed("GET /whitelist/remove/cancel", s.whitelistCancel)
	s.authed("POST /decisions/{id}/undo", s.undo)
	s.authed("POST /decisions/{id}/learn", s.learn)
}

// authed registers a route that needs a signed-in session.
func (s *Server) authed(pattern string, handler http.HandlerFunc) {
	s.mux.Handle(pattern, s.requireSession(handler))
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
