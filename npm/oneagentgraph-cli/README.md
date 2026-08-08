# oneagentgraph-cli

The `oneagentgraph` command, as a prebuilt binary.

```bash
npm install -g oneagentgraph-cli
oneagentgraph --help
```

The binary ships inside a per-platform package that npm selects by `os`/`cpu`, so
there is no compile step and no Rust toolchain to install. The same binary is on
[PyPI](https://pypi.org/project/oneagentgraph-cli/) (`pip install
oneagentgraph-cli`) and [crates.io](https://crates.io/crates/oneagentgraph)
(`cargo install oneagentgraph`).

See [the repository](https://github.com/nickderobertis/oneagentgraph) for what it
does and the contract it implements.
