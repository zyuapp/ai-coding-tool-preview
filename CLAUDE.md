@AGENTS.md

# Testing guide

When a fix changes what a user action does, perform that action again after the fix, and test both the case it fixes and the case it must leave alone. Before calling a behavior a bug, check whether it is the action's intended effect.

Add a test only when a plausible bug would make it fail. Fewer, stronger tests beat many weak ones.
- Do not test constants, copy text, theme tokens or styles, defaults, or anything the type checker already enforces.
- Test a rule once, at the lowest layer that owns it. Search for an existing test and extend it before adding one. When logic moves into a helper, move its tests with it.
- Assert only the fields under test (`matches!`, single-field asserts, `iter().any()`), never whole effect lists, params, or records.
- Do not read source files in tests. Style conventions belong in lint.
- When a feature is removed, delete its tests. Do not add tests that it stays absent.
- UI polish and audit passes change the UI only. They add no tests unless they fix a behavior bug.

# Clean up after yourself

Before ending your turn, tear down everything you started: quit any dev app or server you launched, stop background processes and Docker containers, delete temporary files, profiles, and debug logging, and restore any window or setting you changed.
