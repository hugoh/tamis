package engine

import (
	"context"
	"errors"
	"fmt"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/mail"
)

// ruleTarget is the folder the rules file a good message into, or "" to leave
// it. A script that fails on one message must not stall the pass, so that
// message is left where it is.
func (e *Engine) ruleTarget(
	ctx context.Context,
	folder string,
	message mail.Message,
	verdict judgement,
) string {
	if e.rules == nil || verdict.verdict != VerdictGood {
		return ""
	}

	target, err := e.rules.Folder(ctx, message.Raw, message.From)
	if err != nil {
		e.log.WarnContext(ctx, "rules failed", "folder", folder, "uid", message.UID, "error", err)

		return ""
	}

	if target == folder || !e.targetUsable(ctx, folder, message, target) {
		return ""
	}

	return target
}

// targetUsable is false for a folder that is missing and may not be created.
// A failed lookup counts as unusable: it leaves the message where it is.
func (e *Engine) targetUsable(
	ctx context.Context,
	folder string,
	message mail.Message,
	target string,
) bool {
	if e.cfg.Rules.CreateFolders {
		return true
	}

	exists, err := e.mail.Exists(target)
	if err != nil {
		e.log.WarnContext(ctx, "rules: look up folder",
			"folder", folder, "uid", message.UID, "target", target, "error", err)

		return false
	}

	if !exists {
		e.log.WarnContext(ctx, "rules: no such folder; leaving the message",
			"folder", folder, "uid", message.UID, "target", target)
	}

	return exists
}

// fileByRule moves a good message to target; in shadow mode it only records
// what it would have done.
func (e *Engine) fileByRule(
	ctx context.Context,
	folder string,
	message mail.Message,
	verdict judgement,
	target string,
) error {
	verdict.reason = ReasonRule
	action := "move:" + target

	if e.cfg.Policy.Mode != config.ModeLive {
		return e.record(ctx, folder, message, verdict, action, false)
	}

	uids := []uint32{message.UID}

	if err := e.mail.Ensure(target); err != nil {
		return fmt.Errorf("file by rule: %w", err)
	}

	if err := e.mail.AddKeyword(folder, uids, SortedKeyword); err != nil {
		return fmt.Errorf("file by rule: %w", err)
	}

	if err := e.mail.Move(folder, uids, target); err != nil {
		// Left in place with the keyword, the message would never be filed.
		if undo := e.mail.RemoveKeyword(folder, uids, SortedKeyword); undo != nil {
			err = errors.Join(err, undo)
		}

		return fmt.Errorf("file by rule: %w", err)
	}

	return e.record(ctx, folder, message, verdict, action, true)
}
