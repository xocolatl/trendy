import * as vscode from "vscode";
import {
  LanguageClient,
  type LanguageClientOptions,
  type ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  context.subscriptions.push(
    vscode.commands.registerCommand("trendy.restartServer", restart),
    vscode.workspace.onDidChangeConfiguration(async (event) => {
      if (event.affectsConfiguration("trendy.server.path")) {
        await restart();
      }
    }),
  );
  await start();
}

export async function deactivate(): Promise<void> {
  await stop();
}

async function start(): Promise<void> {
  const command = vscode.workspace
    .getConfiguration("trendy")
    .get<string>("server.path", "trendy-lsp");

  const serverOptions: ServerOptions = { command };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ language: "trendy" }],
  };

  client = new LanguageClient("trendy", "trendy", serverOptions, clientOptions);
  try {
    await client.start();
  } catch (error) {
    client = undefined;
    const message = error instanceof Error ? error.message : String(error);
    void vscode.window.showErrorMessage(
      `trendy: could not start language server \`${command}\`: ${message}. ` +
        "Set `trendy.server.path` to the trendy-lsp executable.",
    );
  }
}

async function stop(): Promise<void> {
  const running = client;
  client = undefined;
  await running?.stop();
}

async function restart(): Promise<void> {
  await stop();
  await start();
}
