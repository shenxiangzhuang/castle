# Desktop fonts

Unmodified static fonts are embedded in the executable on macOS, Windows and Linux.
Regular/bold faces provide consistent Latin/CJK and code metrics without installed-font assumptions.
Source Han Sans CN is Adobe's Simplified Chinese subset (30,926 mapped characters); uncommon
scripts and emoji can still use the platform fallback. The four faces total about 16.5 MiB.

- [Source Han Sans 2.005R](https://github.com/adobe-fonts/source-han-sans/releases/tag/2.005R),
  `SubsetOTF/CN/SourceHanSansCN-{Regular,Bold}.otf` on the release branch.
- [Source Code Pro](https://github.com/adobe-fonts/source-code-pro/tree/release/OTF),
  `SourceCodePro-{Regular,Bold}.otf` on the release branch.

`SHA256SUMS` pins the checked-in bytes. Both fonts use SIL OFL 1.1; the adjacent upstream
license files are included in every distribution. Do not rename or subset these resources
without reviewing the reserved-name and license requirements.

Verify resources with `cd crates/desktop/assets/fonts && shasum -a 256 -c SHA256SUMS`.
