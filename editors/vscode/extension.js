const { workspace, window } = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");
const path = require("path");
const fs = require("fs");

let client;

function activate(context) {
    // Look for nuvola-lsp in several locations
    const candidates = [
        // Built from source (stage0)
        path.join(context.extensionPath, "..", "..", "stage0", "target", "release", "nuvola-lsp"),
        // System PATH
        "nuvola-lsp",
    ];

    let serverPath = null;
    for (const c of candidates) {
        if (path.isAbsolute(c) && fs.existsSync(c)) {
            serverPath = c;
            break;
        }
    }

    if (!serverPath) {
        // Try the non-absolute one (expects it on PATH)
        serverPath = "nuvola-lsp";
    }

    const serverOptions = {
        run:   { command: serverPath, transport: TransportKind.stdio },
        debug: { command: serverPath, transport: TransportKind.stdio },
    };

    const clientOptions = {
        documentSelector: [{ scheme: "file", language: "nuvola" }],
        synchronize: {
            fileEvents: workspace.createFileSystemWatcher("**/*.nvl"),
        },
    };

    client = new LanguageClient(
        "nuvolaLsp",
        "Nuvola Language Server",
        serverOptions,
        clientOptions
    );

    client.start();
}

function deactivate() {
    if (client) {
        return client.stop();
    }
}

module.exports = { activate, deactivate };
