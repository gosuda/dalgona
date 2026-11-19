### Workstream: Code symbols

Agents list the definitions of a file or tree, read one definition body by name, and run a tree-sitter query, all through the `search` tool in both products. Parsing runs only in the sandboxed search child, through one package, `internal/syntax`, which is the only importer of `github.com/odvcencio/gotreesitter`. Every row carries a path and a line or line range that the `read` and `patch` tools can use. Each call has a file cap, a count cap, a per-parse timeout, a whole-call budget, and a hard kill at the child deadline. Every omission is named in a footer. A parse that stops gives no rows. A parse with syntax errors keeps its rows and says so. The row forms, error texts, footers and bounds are the contract, and fixtures and benchmarks check them.

