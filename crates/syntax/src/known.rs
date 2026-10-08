//! The languages clackatoo knows where to get: each grammar's own release,
//! compiled to WebAssembly by its authors, and the highlight queries from
//! its repository at the same version, as tried with clackatoo. Each file
//! is checked against the SHA-256 here, so a release changed since can't
//! slip in.
//!
//! Where a grammar builds on another's queries, as TypeScript's on
//! JavaScript's, they're given in the order its `tree-sitter.json` gives.

use crate::install::{Download, Known};

/// Every language [`install`](crate::install::install) can install.
pub const KNOWN: &[Known] = &[
    Known {
        name: "bash",
        grammar: "bash",
        source: "tree-sitter/tree-sitter-bash v0.25.1",
        extensions: &["sh", "bash"],
        filenames: &[".bashrc", ".bash_profile", ".profile"],
        wasm: Download {
            url: "https://github.com/tree-sitter/tree-sitter-bash/releases/download/v0.25.1/tree-sitter-bash.wasm",
            sha256: "8292919c88a0f7d3fb31d0cd0253ca5a9531bc1ede82b0537f2c63dd8abe6a7a",
        },
        highlights: &[Download {
            url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-bash/v0.25.1/queries/highlights.scm",
            sha256: "b74220d954f485b7626d2b2b61f37b522e12eb1830803e388e57dd797dc99f11",
        }],
    },
    Known {
        name: "c",
        grammar: "c",
        source: "tree-sitter/tree-sitter-c v0.24.2",
        extensions: &["c", "h"],
        filenames: &[],
        wasm: Download {
            url: "https://github.com/tree-sitter/tree-sitter-c/releases/download/v0.24.2/tree-sitter-c.wasm",
            sha256: "83e8d7902b9d7f8c7c5cd4bd9acb5c7eb5faf42c09f85546b183964d3b5f48f9",
        },
        highlights: &[Download {
            url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-c/v0.24.2/queries/highlights.scm",
            sha256: "3378d854dda695b2b282b9468247524ed4271ef74af691033d2e36883379409c",
        }],
    },
    Known {
        name: "css",
        grammar: "css",
        source: "tree-sitter/tree-sitter-css v0.25.0",
        extensions: &["css"],
        filenames: &[],
        wasm: Download {
            url: "https://github.com/tree-sitter/tree-sitter-css/releases/download/v0.25.0/tree-sitter-css.wasm",
            sha256: "8a23977fe271357cce6f254ef88c9bebf3854602d8046605aef6a45c02135c59",
        },
        highlights: &[Download {
            url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-css/v0.25.0/queries/highlights.scm",
            sha256: "23f7948a3817a0d06d7120158c3bee4ec5b58daa77e524985a4738b364db17b2",
        }],
    },
    Known {
        name: "go",
        grammar: "go",
        source: "tree-sitter/tree-sitter-go v0.25.0",
        extensions: &["go"],
        filenames: &[],
        wasm: Download {
            url: "https://github.com/tree-sitter/tree-sitter-go/releases/download/v0.25.0/tree-sitter-go.wasm",
            sha256: "9504573f352b20be7f2f1911754d710622aedc15afff16d5ed8fb5645681aee7",
        },
        highlights: &[Download {
            url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-go/v0.25.0/queries/highlights.scm",
            sha256: "81182c986547eba7fa6316e82dfd621fb13b8fc89efac85432aee51a48ed0896",
        }],
    },
    Known {
        name: "html",
        grammar: "html",
        source: "tree-sitter/tree-sitter-html v0.23.2",
        extensions: &["html", "htm"],
        filenames: &[],
        wasm: Download {
            url: "https://github.com/tree-sitter/tree-sitter-html/releases/download/v0.23.2/tree-sitter-html.wasm",
            sha256: "c48fcd82c7ea8bf943180088ba7f28c48b2bb5287874179168bf9d31e394cf85",
        },
        highlights: &[Download {
            url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-html/v0.23.2/queries/highlights.scm",
            sha256: "1ebb3811a8cdc054385b847a3aac6fbf7079faefd5b3dfab5bbad256cd5afdcf",
        }],
    },
    Known {
        name: "javascript",
        grammar: "javascript",
        source: "tree-sitter/tree-sitter-javascript v0.25.0",
        extensions: &["js", "mjs", "cjs", "jsx"],
        filenames: &[],
        wasm: Download {
            url: "https://github.com/tree-sitter/tree-sitter-javascript/releases/download/v0.25.0/tree-sitter-javascript.wasm",
            sha256: "5fb488d0cabb4775a594bab85682de5ad6ce83c0d6ac997a9f82dd084d571240",
        },
        highlights: &[
            Download {
                url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-javascript/v0.25.0/queries/highlights.scm",
                sha256: "d3630ae6dc9b2b27b230b5f8bb92b05cd491fb12bff353dae62a0a6d780461ee",
            },
            Download {
                url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-javascript/v0.25.0/queries/highlights-jsx.scm",
                sha256: "0cad1712acaf40804bc5dc7c8476858752c1f549cdf7d8c63c747317be0f8fa6",
            },
            Download {
                url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-javascript/v0.25.0/queries/highlights-params.scm",
                sha256: "bbe7a58cacfb1c954cb0eab44779fd80421ed59b762b975723447207635de50d",
            },
        ],
    },
    Known {
        name: "json",
        grammar: "json",
        source: "tree-sitter/tree-sitter-json v0.24.8",
        extensions: &["json"],
        filenames: &[],
        wasm: Download {
            url: "https://github.com/tree-sitter/tree-sitter-json/releases/download/v0.24.8/tree-sitter-json.wasm",
            sha256: "d2119fb98d5912719b13f9458574f8608d2d29dfbe45f6be1f860ea1fe2a2405",
        },
        highlights: &[Download {
            url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-json/v0.24.8/queries/highlights.scm",
            sha256: "0511524465b56aed122580792254e68b6abbbfde7119f1d02b135acbe278233f",
        }],
    },
    Known {
        name: "lua",
        grammar: "lua",
        source: "tree-sitter-grammars/tree-sitter-lua v0.5.0",
        extensions: &["lua", "luau"],
        filenames: &[],
        wasm: Download {
            url: "https://github.com/tree-sitter-grammars/tree-sitter-lua/releases/download/v0.5.0/tree-sitter-lua.wasm",
            sha256: "df08a1704e504c70b8dba4a3e6f8e0c99a4fb94e1b1693d2969f53141d09f0d4",
        },
        highlights: &[Download {
            url: "https://raw.githubusercontent.com/tree-sitter-grammars/tree-sitter-lua/v0.5.0/queries/highlights.scm",
            sha256: "d2625fb712357f86386878602d18845f9f3e90fee81f02a5ecd87be6512918d1",
        }],
    },
    Known {
        name: "python",
        grammar: "python",
        source: "tree-sitter/tree-sitter-python v0.25.0",
        extensions: &["py", "pyi"],
        filenames: &[],
        wasm: Download {
            url: "https://github.com/tree-sitter/tree-sitter-python/releases/download/v0.25.0/tree-sitter-python.wasm",
            sha256: "16108b50df4ee9a30168794252ab55e7c93bfc5765d7fa0aa3e335752c515f47",
        },
        highlights: &[Download {
            url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-python/v0.25.0/queries/highlights.scm",
            sha256: "a6708f209381618e2b398972c8f1ccd892f0c064eab35a2a3f911c3e22e79a7e",
        }],
    },
    Known {
        name: "rust",
        grammar: "rust",
        source: "tree-sitter/tree-sitter-rust v0.24.2",
        extensions: &["rs"],
        filenames: &[],
        wasm: Download {
            url: "https://github.com/tree-sitter/tree-sitter-rust/releases/download/v0.24.2/tree-sitter-rust.wasm",
            sha256: "24c89bd9252255e4aebbcbd7d2d308bd92c86dd95a130fdc80efa49577b8d738",
        },
        highlights: &[Download {
            url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-rust/v0.24.2/queries/highlights.scm",
            sha256: "0f0343107f14a7690157f51090a979eb8f8bfe4eada7c61763ddb4c54b1311d1",
        }],
    },
    Known {
        name: "toml",
        grammar: "toml",
        source: "tree-sitter-grammars/tree-sitter-toml v0.7.0",
        extensions: &["toml"],
        filenames: &["Cargo.lock"],
        wasm: Download {
            url: "https://github.com/tree-sitter-grammars/tree-sitter-toml/releases/download/v0.7.0/tree-sitter-toml.wasm",
            sha256: "1ac6a83826c35a68857f8325e00c78f6bcbef4eb6db3931a6cf3041f76e5e09f",
        },
        highlights: &[Download {
            url: "https://raw.githubusercontent.com/tree-sitter-grammars/tree-sitter-toml/v0.7.0/queries/highlights.scm",
            sha256: "2fb5c61d33a70389312254c9c392e2c5dd6313d958d6e5f8f74cadd5e0811707",
        }],
    },
    Known {
        name: "typescript",
        grammar: "typescript",
        source: "tree-sitter/tree-sitter-typescript v0.23.2",
        extensions: &["ts", "mts", "cts"],
        filenames: &[],
        wasm: Download {
            url: "https://github.com/tree-sitter/tree-sitter-typescript/releases/download/v0.23.2/tree-sitter-typescript.wasm",
            sha256: "778025db5a8be0e70f8ccc3671e486dfeddd048c25d9e8a70c26de2e1bf6f97d",
        },
        highlights: &[
            Download {
                url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-typescript/v0.23.2/queries/highlights.scm",
                sha256: "e0c35adb819127bfd4f853fac5419e7d8ba44760246201d04a4a5ce0228a10c5",
            },
            Download {
                url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-javascript/v0.23.1/queries/highlights.scm",
                sha256: "d3630ae6dc9b2b27b230b5f8bb92b05cd491fb12bff353dae62a0a6d780461ee",
            },
        ],
    },
    Known {
        name: "tsx",
        grammar: "tsx",
        source: "tree-sitter/tree-sitter-typescript v0.23.2",
        extensions: &["tsx"],
        filenames: &[],
        wasm: Download {
            url: "https://github.com/tree-sitter/tree-sitter-typescript/releases/download/v0.23.2/tree-sitter-tsx.wasm",
            sha256: "79e5da75ea62855a0cd67177685f0164eac87d5f630b3cbe1e0a099751ad30f8",
        },
        highlights: &[
            Download {
                url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-typescript/v0.23.2/queries/highlights.scm",
                sha256: "e0c35adb819127bfd4f853fac5419e7d8ba44760246201d04a4a5ce0228a10c5",
            },
            Download {
                url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-javascript/v0.23.1/queries/highlights-jsx.scm",
                sha256: "0cad1712acaf40804bc5dc7c8476858752c1f549cdf7d8c63c747317be0f8fa6",
            },
            Download {
                url: "https://raw.githubusercontent.com/tree-sitter/tree-sitter-javascript/v0.23.1/queries/highlights.scm",
                sha256: "d3630ae6dc9b2b27b230b5f8bb92b05cd491fb12bff353dae62a0a6d780461ee",
            },
        ],
    },
    Known {
        name: "yaml",
        grammar: "yaml",
        source: "tree-sitter-grammars/tree-sitter-yaml v0.7.2",
        extensions: &["yml", "yaml"],
        filenames: &[],
        wasm: Download {
            url: "https://github.com/tree-sitter-grammars/tree-sitter-yaml/releases/download/v0.7.2/tree-sitter-yaml.wasm",
            sha256: "0a0e5ebcfeb0b2bf272d071396eeed107b2c4b2617b69d5959b13f78b357a4d6",
        },
        highlights: &[Download {
            url: "https://raw.githubusercontent.com/tree-sitter-grammars/tree-sitter-yaml/v0.7.2/queries/highlights.scm",
            sha256: "2fbfbbd5ca0928d0a7e09e59f8f2964b14a328a7ee0ce799270d9013dcce49a0",
        }],
    },
];
