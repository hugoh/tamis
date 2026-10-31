//go:build unix

package cli

import (
	"fmt"
	"os"
	"syscall"
)

// fileStream names a file the way systemd names the journal's stream in
// $JOURNAL_STREAM: device and inode.
func fileStream(file *os.File) (string, bool) {
	info, err := file.Stat()
	if err != nil {
		return "", false
	}

	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok {
		return "", false
	}

	dev := uint64(stat.Dev) //nolint:unconvert,gosec,nolintlint // differs by OS; never negative
	ino := uint64(stat.Ino) //nolint:unconvert // the type differs by OS

	return fmt.Sprintf("%d:%d", dev, ino), true
}
