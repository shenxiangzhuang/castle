# 跨平台 UI 验证 · Castle desktop

中英文混排：Linux、Windows、macOS 应保持清晰、稳定的布局与一致的交互。
Regular text, **bold text**, *italic text*, `inline_code(中文参数)` and a [link](https://example.com).

这是一段用于窄窗口换行的长文字。缩放、切换主题、收起侧栏后，文字不应被裁剪或覆盖。
English words beside 中文字符 should wrap naturally and selection should preserve the exact source.

## 代码与数学

```rust
// 中文注释与 ASCII 缩进都应该保持可读、可复制。
fn main() {
    let message = "Hello, Castle / 你好，Castle";
    println!("{message}");
}
```

Inline math \(x_t\), \(K,V\), and \(q_t\) should share the surrounding baseline.

$$
E = mc^2 \qquad \sum_{i=1}^{n} i = \frac{n(n+1)}{2}
$$

| 平台 | 标准窗口 | 缩放 |
| --- | --- | --- |
| Ubuntu / X11 or XWayland | 1180 × 720 | 100% / 125% / 150% / 200% |
| Windows / WebView2 | 1180 × 720 | 100% / 125% / 150% / 200% |
| macOS / WKWebView | 1180 × 720 | 系统提供的缩放 |

## 内嵌 HTML

```html
<style>
body { color: #334155; background: #f1f5f9; }
.card { padding: 18px; border: 1px solid #94a3b8; border-radius: 12px; }
button, input { font: inherit; }
</style>
<div class="card">
  <strong>Interactive preview / 交互预览</strong>
  <p>Move the slider, press the button, then scroll away and back.</p>
  <input aria-label="Preview value" type="range" min="0" max="100" value="25" oninput="value.textContent=this.value">
  <output id="value">25</output>
  <button onclick="count.textContent=Number(count.textContent)+1">Count: <span id="count">0</span></button>
</div>
```

## 长列表

1. 第 1 行：中英文、数字 001、`code_1`，用于检查滚动与选择。
2. 第 2 行：中英文、数字 002、`code_2`，用于检查滚动与选择。
3. 第 3 行：中英文、数字 003、`code_3`，用于检查滚动与选择。
4. 第 4 行：中英文、数字 004、`code_4`，用于检查滚动与选择。
5. 第 5 行：中英文、数字 005、`code_5`，用于检查滚动与选择。
6. 第 6 行：中英文、数字 006、`code_6`，用于检查滚动与选择。
7. 第 7 行：中英文、数字 007、`code_7`，用于检查滚动与选择。
8. 第 8 行：中英文、数字 008、`code_8`，用于检查滚动与选择。
9. 第 9 行：中英文、数字 009、`code_9`，用于检查滚动与选择。
10. 第 10 行：中英文、数字 010、`code_10`，用于检查滚动与选择。
11. 第 11 行：中英文、数字 011、`code_11`，用于检查滚动与选择。
12. 第 12 行：中英文、数字 012、`code_12`，用于检查滚动与选择。
13. 第 13 行：中英文、数字 013、`code_13`，用于检查滚动与选择。
14. 第 14 行：中英文、数字 014、`code_14`，用于检查滚动与选择。
15. 第 15 行：中英文、数字 015、`code_15`，用于检查滚动与选择。
16. 第 16 行：中英文、数字 016、`code_16`，用于检查滚动与选择。
17. 第 17 行：中英文、数字 017、`code_17`，用于检查滚动与选择。
18. 第 18 行：中英文、数字 018、`code_18`，用于检查滚动与选择。
19. 第 19 行：中英文、数字 019、`code_19`，用于检查滚动与选择。
20. 第 20 行：中英文、数字 020、`code_20`，用于检查滚动与选择。
21. 第 21 行：中英文、数字 021、`code_21`，用于检查滚动与选择。
22. 第 22 行：中英文、数字 022、`code_22`，用于检查滚动与选择。
23. 第 23 行：中英文、数字 023、`code_23`，用于检查滚动与选择。
24. 第 24 行：中英文、数字 024、`code_24`，用于检查滚动与选择。
25. 第 25 行：中英文、数字 025、`code_25`，用于检查滚动与选择。
26. 第 26 行：中英文、数字 026、`code_26`，用于检查滚动与选择。
27. 第 27 行：中英文、数字 027、`code_27`，用于检查滚动与选择。
28. 第 28 行：中英文、数字 028、`code_28`，用于检查滚动与选择。
29. 第 29 行：中英文、数字 029、`code_29`，用于检查滚动与选择。
30. 第 30 行：中英文、数字 030、`code_30`，用于检查滚动与选择。
31. 第 31 行：中英文、数字 031、`code_31`，用于检查滚动与选择。
32. 第 32 行：中英文、数字 032、`code_32`，用于检查滚动与选择。
33. 第 33 行：中英文、数字 033、`code_33`，用于检查滚动与选择。
34. 第 34 行：中英文、数字 034、`code_34`，用于检查滚动与选择。
35. 第 35 行：中英文、数字 035、`code_35`，用于检查滚动与选择。
36. 第 36 行：中英文、数字 036、`code_36`，用于检查滚动与选择。
37. 第 37 行：中英文、数字 037、`code_37`，用于检查滚动与选择。
38. 第 38 行：中英文、数字 038、`code_38`，用于检查滚动与选择。
39. 第 39 行：中英文、数字 039、`code_39`，用于检查滚动与选择。
40. 第 40 行：中英文、数字 040、`code_40`，用于检查滚动与选择。
