# Third-party notices

The root MIT license applies to original exported CONST API source. It does not
relicense dependencies, vendor assets, or the additional private modules in
official release binaries. Dependency versions are pinned in
`client/package-lock.json` and `client/src-tauri/Cargo.lock`.

Most declared dependencies use MIT, Apache-2.0, BSD or ISC terms. Some transitive
components use MPL-2.0, Unicode, certificate-data or other licenses. Preserve
their license texts and notices, and provide covered source where required when
redistributing a binary. A package's license metadata is not a complete binary
distribution audit; check the dependencies and native libraries for the target OS.

## Assets and model metadata

Tool logos in `client/src/assets/tool-icons/` identify compatible third-party
products and remain the property of their respective owners; the root MIT
license does not relicense these marks. The OmniRoute icon has its accompanying
MIT notice in `client/src/assets/tool-icons/omniroute.LICENSE.txt`.
Product names identify compatible third-party tools. MIT does not grant
trademark rights or establish endorsement. Upstream services retain their own
service terms; this project's license does not grant access to them.

Bundled model metadata incorporates public catalog data from
[LiteLLM](https://github.com/BerriAI/litellm). Its MIT notice is retained below.
No LiteLLM enterprise source is included.

## LiteLLM — MIT License

Copyright (c) 2023 Berri AI

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
