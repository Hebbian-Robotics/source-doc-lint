# Source Doc Lint

Source Doc Lint checks source comments and documentation in Rust, Python,
TypeScript, and TSX. It runs as a single Rust binary and uses ast-grep's Rust
libraries to inspect syntax.

## Install and run

Download your platform archive from [GitHub Releases], extract it, and put
`source-doc-lint` on your PATH. If you prefer a source build, use Cargo and a
C compiler:

```sh
cargo install --git https://github.com/Hebbian-Robotics/source-doc-lint --tag v0.1.0 --locked
```

For a directly installed release binary, run `source-doc-lint update --check`
to check for a new version, or `source-doc-lint update` to download, verify,
and install it. Use the original installation tool for a managed copy.

Run from the directory containing `source-doc-lint.toml`:

```sh
source-doc-lint
source-doc-lint src tests
source-doc-lint --json src
```

Use `--config PATH` to select a different config file. Policy paths are relative
to the directory containing that file. Scan paths are relative to the working
directory and default to the current directory. Hidden source files are
included. The scanner respects Git and ignore files and skips symlink entries.
To scan a path named `update`, pass it as `./update`.

The exit code is 0 for a clean scan, 1 for violations, and 2 for a tool or
configuration error. JSON reports contain `reason` and `findings`, with `error`
when the scan cannot finish. JSON line and column indices start at zero. Text
diagnostics start at one.

## Configure rules

Both policy rules are opt-in. Include the corresponding TOML table to enable
a rule. With no rule tables, a scan reports zero violations.

```toml
reason = "Keep implementation rationale close to the code it explains."
exclude = ["vendor/**"]

[classification]
labels = ["WHY", "INVARIANT", "TODO"]
exclude = ["examples/**"]
extra_directives = ["^// my-tool:"]

[public_interface]
directories = ["sdk", "src/public"]
kinds = ["docstring", "doc-comment", "doc-attribute", "schema-description"]
```

- **Classification** requires a configured label followed by `:` and an
  explanation. Consecutive standalone line comments at identical indentation
  form a paragraph. Inline and block comments are checked independently.
  Common lint and formatter directives, Python shebangs and encoding hints,
  and license headers are exempt. Additional directive patterns are Rust
  regular expressions checked against the whole comment text.
- **Public interface** permits the selected documentation kinds in the listed
  directories. It covers all kinds by default when enabled. Directory matches
  use path components. For example, `sdk` allows `sdk/src/lib.rs`, while
  `sdk-extras/lib.rs` and `nested/sdk/lib.rs` are outside that boundary.
  Classification exclusions have no effect on this rule.
- **Exclusions** use glob patterns. Use the top-level `exclude` for paths that
  should be outside both rules. Generated source is checked unless excluded
  by the applicable rule or an ignore file.

## Detection and limits

- Python module, class, and function docstrings are recognized by their syntax,
  including async functions and concatenated and parenthesized string literals.
  Assigned strings, bytes, and f-strings do not count as docstrings.
- Rust documentation comments and `doc = …` attributes are recognized. The
  schema rule recognizes `schemars(description = …)`.
- TypeScript and TSX JSDoc is recognized. The schema rule recognizes Zod
  `.describe(…)`, `.meta({ description: … })`, and `.meta(metadata)` when
  `metadata` is a preceding, same-scope `const` object with a literal
  `description` key. Static imports from `zod`, `zod/v3`, and `zod/v4`,
  common schema factories, chained calls, and local schema aliases are
  supported. Zod receiver analysis assumes unshadowed imports and schema aliases.
- The Python schema rule recognizes Pydantic `Field(description=…)` and literal
  `json_schema_extra` dictionaries with a `description` on `Field` or
  `ConfigDict`. Direct, qualified, and aliased imports are supported, including
  function-local imports. Parameter and assignment shadowing suppress uncertain
  bindings. Python semantic checks use the RustPython parser. Syntax it cannot
  parse causes a tool error when those checks are enabled.
- Python files are supported up to 383 distinct indentation widths whenever a
  rule checks them. Python semantic checks also support up to 1,024 levels of
  nesting, where a level is a statement inside a compound statement, an `elif`
  clause, an expression inside an expression, or a pattern inside a pattern.
  Deeper files cause a nesting-depth tool error that names the file.

Schema detection is file-local. It does not resolve dynamic imports,
re-exports, wrapper functions, computed or spread metadata, Python assignment
aliases, star imports, comprehension scopes, or `global`/`nonlocal` rebinding.
Rust `cfg_attr` and Schemars `extend` are outside the supported syntax.
Ordinary application descriptions, CLI help, and unrelated methods named
`describe` are outside the schema rules.

## Contribute

See [Contributing](CONTRIBUTING.md) for setup, checks, and how to report bugs.

[GitHub Releases]: https://github.com/Hebbian-Robotics/source-doc-lint/releases
