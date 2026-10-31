package web

import (
	"net/http"
	"os"

	"github.com/hugoh/tamis/internal/rules"
)

const (
	pageRules = "rules"

	// maxTestMessage matches how much of a message the engine hands the rules.
	maxTestMessage = 256 << 10
)

type rulesBody struct {
	Path, Source string
	// Problem is why the script can't be used, or empty when it can.
	Problem string
}

type rulesResult struct {
	Problem, Folder string
}

func (s *Server) rules(resp http.ResponseWriter, _ *http.Request) {
	body := rulesBody{Path: s.cfg.Rules.Script}

	if body.Path != "" {
		source, err := os.ReadFile(body.Path)
		if err == nil {
			body.Source = string(source)
			_, err = rules.Load(body.Path)
		}

		if err != nil {
			body.Problem = err.Error()
		}
	}

	s.render(resp, pageRules, view{Title: pageRules, Nav: pageRules, Body: body})
}

func (s *Server) rulesTest(resp http.ResponseWriter, req *http.Request) {
	req.Body = http.MaxBytesReader(resp, req.Body, maxTestMessage)

	var result rulesResult

	switch message := req.FormValue("message"); {
	case s.cfg.Rules.Script == "":
		result.Problem = "No rules script configured (rules.script)."
	case message == "":
		result.Problem = "Paste a message, headers included."
	default:
		loaded, err := rules.Load(s.cfg.Rules.Script)
		if err == nil {
			raw := []byte(message)
			result.Folder, err = loaded.Folder(req.Context(), raw, rules.SenderOf(raw))
		}

		if err != nil {
			result.Problem = err.Error()
		}
	}

	s.renderFragment(resp, pageRules, "result", result)
}
