// Connects VS Code to the DAP server of `rs6809 --dap`. No dependencies: the
// emulator itself is the debug adapter.
const vscode = require("vscode");

function activate(context) {
  context.subscriptions.push(
    vscode.debug.registerDebugAdapterDescriptorFactory("rs6809", {
      createDebugAdapterDescriptor(session) {
        const c = session.configuration;
        return new vscode.DebugAdapterServer(c.port || 6810, c.host || "127.0.0.1");
      },
    })
  );
}

function deactivate() {}

module.exports = { activate, deactivate };
