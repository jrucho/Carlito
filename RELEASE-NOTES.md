# Carlito 0.2.1 — fast handwritten answers

- The fast Carlito 2 configuration is now the default public release.
- Gemini Flash-Lite answers directly, avoiding the extra request delays from
  the preceding release.
- Paginated handwritten answers and simple line diagrams.
- Lightweight session context from recent assistant replies, cleared on exit.
- Two-finger downward swipe starts a fresh session; five fingers exit.
- AppLoad name remains Carlito.
- Compiled Paper Pro Move bundle: no coding or compilation required to install.
- Your API key remains private and is not included in downloads.

Answers use Gemini's model knowledge and do not independently verify current
facts. Response times vary with network conditions and model load.

Verification: host tests passed and the Move aarch64 takeover binary compiled;
dynamic dependencies and release contents were checked. This new compiled
release has not yet had an on-device writing/exit smoke test. RM2 is not supported.
