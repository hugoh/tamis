// Command tamis is a server-side spam filter that complements a mail
// provider's own filtering by watching remote IMAP/JMAP folders.
package main

import (
	"os"

	"github.com/hugoh/tamis/internal/cli"
)

// version is overridden at build time via -ldflags.
var version = "dev"

func main() {
	os.Exit(cli.Run(os.Args[1:], os.Stdout, os.Stderr, version))
}
