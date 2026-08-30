package main

import (
	"embed"
	"log"
	"os"
	"path/filepath"

	"datafactory-next/studio/application"
	"github.com/wailsapp/wails/v2"
	"github.com/wailsapp/wails/v2/pkg/options"
	"github.com/wailsapp/wails/v2/pkg/options/assetserver"
)

//go:embed all:frontend/dist
var assets embed.FS

func main() {
	engine := os.Getenv("DF_ENGINE_PATH")
	if engine == "" {
		exe, _ := os.Executable()
		engine = filepath.Join(filepath.Dir(exe), "df-engine.exe")
	}
	app := application.New(engine)
	if err := wails.Run(&options.App{Title: "DataFactory Studio", Width: 1360, Height: 860, MinWidth: 1000, MinHeight: 680, AssetServer: &assetserver.Options{Assets: assets}, OnStartup: app.Startup, OnShutdown: app.Shutdown, Bind: []interface{}{app}}); err != nil {
		log.Fatal(err)
	}
}
