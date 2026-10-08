# Carlito 0.2.0 — live web search

- Standalone public Carlito project for Paper Pro Move.
- Native Gemini Interactions API with Google Search enabled by default.
- Quota fallback explicitly labels answers that could not be web-verified.
- Citation titles and URLs displayed with answers.
- Paginated handwritten answers and simple line diagrams.
- Two-finger downward swipe starts a fresh session; five fingers exit.
- Move aarch64 install bundle: no compilation required to install.
- Keys/configuration and proprietary vendor libraries are excluded from downloads.

Verification: 14 host tests pass, including mocked search quota fallback and
source citation handling; the aarch64 takeover binary compiled and its dynamic
dependencies were inspected. This release has not yet been smoke-tested on the
connected Move hardware. RM2 is not supported by this release.

Google Search needs an API project with available grounding quota. If quota
rejects the request, Carlito says so and answers without live verification.
