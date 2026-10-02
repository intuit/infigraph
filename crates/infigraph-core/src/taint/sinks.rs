pub struct TaintSink {
    pub kind: &'static str,
    pub category: &'static str,
    pub patterns: &'static [&'static str],
    pub extensions: Option<&'static [&'static str]>,
}

pub static TAINT_SINKS: &[TaintSink] = &[
    // SQL execution
    TaintSink {
        kind: "SqlQuery",
        category: "SqlInjection",
        patterns: &[
            "execute(",
            "cursor.execute(",
            "executemany(",
            "raw_query(",
            "rawquery(",
            "executequery(",
            "executeupdate(",
            "connection.query(",
            "db.query(",
            "db.exec(",
            "db.queryrow(",
            "sqlcommand(",
            "executereader(",
            "executenonquery(",
            "query!(",
            "sqlx::query(",
        ],
        extensions: None,
    },
    // Command execution
    TaintSink {
        kind: "CommandExec",
        category: "CommandInjection",
        patterns: &[
            "os.system(",
            "os.popen(",
            "subprocess.call(",
            "subprocess.run(",
            "subprocess.popen(",
            "subprocess.check_output(",
            "exec(",
            "child_process.exec(",
            "child_process.execsync(",
            "runtime.getruntime().exec(",
            "processbuilder(",
            "process.start(",
            "cmd.run(",
            "cmd.output(",
            "std::process::command::new(",
        ],
        extensions: None,
    },
    // HTML rendering (XSS sinks)
    TaintSink {
        kind: "HtmlRender",
        category: "XssRisk",
        patterns: &[
            "innerhtml",
            "outerhtml",
            "dangerouslysetinnerhtml",
            "document.write(",
            "document.writeln(",
            "mark_safe(",
            "|safe",
            "markup(",
            "html.raw(",
            "template.html(",
        ],
        extensions: None,
    },
    // File system access
    TaintSink {
        kind: "FileAccess",
        category: "PathTraversal",
        patterns: &[
            "open(",
            "os.path.join(",
            "path.join(",
            "readfile(",
            "writefile(",
            "fs.readfile(",
            "fs.writefile(",
            "files.write(",
            "files.read(",
            "file.writealltext(",
            "file.readalltext(",
            "os.open(",
            "os.create(",
            "os.writefile(",
            "std::fs::read(",
            "std::fs::write(",
        ],
        extensions: None,
    },
    // Redirect
    TaintSink {
        kind: "Redirect",
        category: "OpenRedirect",
        patterns: &[
            "redirect(",
            "res.redirect(",
            "response.redirect(",
            "location.href",
            "window.location",
            "httpresponseredirect(",
            "sendredirect(",
            "response.redirect(",
            "http.redirect(",
        ],
        extensions: None,
    },
    // Deserialization
    TaintSink {
        kind: "Deserialize",
        category: "InsecureDeserialization",
        patterns: &[
            "pickle.loads(",
            "pickle.load(",
            "yaml.load(",
            "yaml.unsafe_load(",
            "unserialize(",
            "json.parse(",
            "objectinputstream(",
            "readobject(",
            "binaryformatter.deserialize(",
            "json.unmarshal(",
        ],
        extensions: None,
    },
    // LDAP injection
    TaintSink {
        kind: "LdapQuery",
        category: "LdapInjection",
        patterns: &[
            "ldap.search(",
            "ldap_search(",
            "search_s(",
            "directorysearcher(",
            "searchrequest(",
        ],
        extensions: None,
    },
    // XPath injection
    TaintSink {
        kind: "XPathQuery",
        category: "XPathInjection",
        patterns: &[
            "xpath(",
            "evaluate(",
            "selectnodes(",
            "xpathexpression(",
            "xpathnavigator.select(",
        ],
        extensions: None,
    },
];

pub static TAINT_SANITIZERS: &[TaintSanitizer] = &[
    TaintSanitizer {
        category: "SqlInjection",
        patterns: &[
            "parameterize",
            "prepare(",
            "bind_param",
            "sanitize_sql",
            "placeholder",
            "?)",
            "%s)",
            "prepared_statement",
        ],
    },
    TaintSanitizer {
        category: "XssRisk",
        patterns: &[
            "escape_html",
            "html.escape(",
            "cgi.escape(",
            "sanitize(",
            "dompurify",
            "bleach.clean(",
            "encodeuricomponent(",
            "markupsafe.escape(",
            "htmlencoder.encode(",
        ],
    },
    TaintSanitizer {
        category: "CommandInjection",
        patterns: &[
            "shlex.quote(",
            "shell_escape",
            "escapeshellarg(",
            "shell=false",
            "shlex.split(",
        ],
    },
    TaintSanitizer {
        category: "PathTraversal",
        patterns: &[
            "realpath(",
            "abspath(",
            "canonicalize(",
            "path.resolve(",
            "secure_filename(",
            "os.path.basename(",
            "filepath.clean(",
        ],
    },
    TaintSanitizer {
        category: "OpenRedirect",
        patterns: &[
            "url_has_allowed_host(",
            "is_safe_url(",
            "validate_redirect(",
            "safe_redirect(",
        ],
    },
    TaintSanitizer {
        category: "InsecureDeserialization",
        patterns: &["safe_load(", "yaml.safe_load(", "safeloader", "allowlist"],
    },
    TaintSanitizer {
        category: "LdapInjection",
        patterns: &["ldap.filter.escape(", "escape_filter_chars("],
    },
    TaintSanitizer {
        category: "XPathInjection",
        patterns: &["xpath_escape(", "parameterized_xpath("],
    },
];

pub struct TaintSanitizer {
    pub category: &'static str,
    pub patterns: &'static [&'static str],
}
