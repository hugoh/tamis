//go:build !unix

package cli

import "os"

func fileStream(*os.File) (string, bool) { return "", false }
