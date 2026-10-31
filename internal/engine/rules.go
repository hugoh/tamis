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

	if target == folder {
		return ""
	}

	return target
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
