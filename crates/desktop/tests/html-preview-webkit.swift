// Run on macOS: swift crates/desktop/tests/html-preview-webkit.swift
import AppKit
import WebKit

func json(_ value: String) -> String {
    let data = try! JSONSerialization.data(withJSONObject: [value], options: [.fragmentsAllowed])
    return String(data: data, encoding: .utf8)!.dropFirst().dropLast()
        .replacingOccurrences(of: "<", with: "\\u003c")
}

final class PreviewCheck: NSObject, WKScriptMessageHandler, WKNavigationDelegate {
    var webView: WKWebView!
    var state: [String: Any]?
    var rendered = 0
    var checkingFailure = false
    var errors = 0

    func finish(_ error: String? = nil) {
        if let error {
            fputs("FAIL: \(error)\n", stderr)
            exit(1)
        }
        print("HTML preview WebKit: cold publication, module DOM-ready/load, single initialization and module errors passed")
        exit(0)
    }

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let data = message.body as? [String: Any], let kind = data["kind"] as? String else { return }
        if kind == "test-state" { state = data }
        if kind == "error" { errors += 1 }
        if kind == "rendered" {
            rendered += 1
            if checkingFailure {
                guard errors > 0 else { finish("module failure was not reported"); return }
                webView.evaluateJavaScript("document.querySelector('#error').textContent") { value, error in
                    guard error == nil, let text = value as? String, !text.isEmpty else {
                        self.finish("module failure has no visible error"); return
                    }
                    self.finish()
                }
            } else {
                guard errors == 0, state?["dom"] as? Int == 1, state?["load"] as? Int == 1,
                      state?["runs"] as? Int == 1, state?["text"] as? String == "Initialized" else {
                    finish("rendered arrived before module DOM-ready/load initialization: \(String(describing: state))"); return
                }
                // The same completed update must not execute or reveal the page twice.
                webView.evaluateJavaScript("window.previewUpdate(\(json(source)), false, 2)") { _, error in
                    if let error { self.finish(error.localizedDescription); return }
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
                        guard self.rendered == 1 else { self.finish("duplicate completion"); return }
                        self.checkingFailure = true
                        self.rendered = 0
                        self.errors = 0
                        self.webView.loadHTMLString(self.host("<p>Static content</p><script type=module>const invalid = ;</script>", pending: false), baseURL: nil)
                    }
                }
            }
        }
    }

    let source = """
    <p id="status">Waiting</p><script type="module">
    parent.postMessage({kind:'test-state', stage:'module-executed'}, '*');
    window.runs = (window.runs || 0) + 1;
    let dom = 0, load = 0;
    document.addEventListener('DOMContentLoaded', () => {
      dom++; document.querySelector('#status').textContent = 'Initialized';
    });
    window.addEventListener('load', () => {
      load++; parent.postMessage({kind:'test-state', dom, load, runs:window.runs,
        text:document.querySelector('#status').textContent}, '*');
    });
    </script>
    """

    func host(_ source: String, pending: Bool) -> String {
        let directory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("../src/chat/html_preview")
        let boot = try! String(contentsOf: directory.appendingPathComponent("document.js"), encoding: .utf8)
        // Match the production opaque document's CSP; run the actual host and bootstrap assets.
        let document = """
        <!doctype html><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'"><script>\(boot)</script>
        """
        return try! String(contentsOf: directory.appendingPathComponent("host.html"), encoding: .utf8)
            .replacingOccurrences(of: "__GENERATION__", with: "1")
            .replacingOccurrences(of: "__STREAMING__", with: pending ? "true" : "false")
            .replacingOccurrences(of: "__TOKEN__", with: json("webkit-test"))
            .replacingOccurrences(of: "__DARK__", with: "false")
            .replacingOccurrences(of: "__DOCUMENT__", with: json(document))
            .replacingOccurrences(of: "__SOURCE__", with: json(source))
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        if checkingFailure { return }
        // Let the empty iframe become ready while source preparation is still pending.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
            guard self.rendered == 0 else { self.finish("published an unprepared empty document"); return }
            self.webView.evaluateJavaScript("window.previewUpdate(\(json(self.source)), false, 2)") { _, error in
                if let error { self.finish(error.localizedDescription) }
            }
        }
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let check = PreviewCheck()
let config = WKWebViewConfiguration()
config.userContentController.add(check, name: "check")
config.userContentController.addUserScript(WKUserScript(source: """
window.ipc = {postMessage: data => webkit.messageHandlers.check.postMessage(JSON.parse(data))};
window.addEventListener('message', event => {
  if (event.source === document.querySelector('iframe')?.contentWindow &&
      ['test-state','rendered'].includes(event.data?.kind))
    webkit.messageHandlers.check.postMessage(event.data);
});
""", injectionTime: .atDocumentStart, forMainFrameOnly: true))
check.webView = WKWebView(frame: NSRect(x: 0, y: 0, width: 640, height: 240), configuration: config)
check.webView.navigationDelegate = check
DispatchQueue.main.asyncAfter(deadline: .now() + 20) {
    check.finish("timed out: failureCase=\(check.checkingFailure), errors=\(check.errors), state=\(String(describing: check.state))")
}
check.webView.loadHTMLString(check.host("", pending: true), baseURL: nil)
app.run()
