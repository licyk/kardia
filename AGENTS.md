# Repository Rules

- Any breaking refactoring is allowed; backward compatibility is not required. Do not add compatibility layers or retain obsolete interfaces solely to preserve existing behavior.
- Avoid excessive checks and overly strict field validation. Validate only what is necessary for correct operation, and tolerate unknown fields in configuration files and upstream data instead of rejecting otherwise usable input.
- Do not add complexity for speculative, extremely unlikely race conditions or security edge cases. If you believe such complexity is necessary, first explain exactly what could go wrong, how likely it is, and how much additional code it would require. Proceed only after I approve. Do not silently expand the scope of changes.
- Validate external data only once, at the entry boundary. Pass values of known types into internal code, and do not repeat defensive checks internally.
- Keep the main function focused on the normal flow. Put validation and implementation details in small helper functions below it.
- Write comments only for non-obvious constraints and counterintuitive behavior. Do not comment on self-explanatory code.
- Do not manually edit generated files. Change the source and regenerate them.
- Test the real implementation. Do not duplicate business logic in tests. Minimize the use of mocks.
- Follow the repository's existing workflows. Do not introduce tools or workflows the repository does not already use.
- Do not prematurely extract functions that are used only once. Extract a function only when it will be reused, encapsulates truly complex logic, or represents a concept that can be clearly named.
