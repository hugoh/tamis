package config

import (
	"context"
	"time"
)

// PasswordWithin is Password with the timeout chosen by the test.
func (a Account) PasswordWithin(ctx context.Context, timeout time.Duration) (string, error) {
	return a.passwordWithin(ctx, timeout)
}
