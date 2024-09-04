package main

import (
	"log/slog"
	"os"

	nsc "github.com/wasmCloud/bootstrap-hierarchy/internal"
)

func main() {
	err := nsc.Bootstrap("work")
	if err != nil {
		slog.Error("failed to bootstrap", slog.String("error", err.Error()))
		os.Exit(1)
	}
}
