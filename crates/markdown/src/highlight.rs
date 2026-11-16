//! Syntax colouring by role, ported from `renderer/diff/highlight.ts`: Shiki's tokenizer over the
//! grammars it bundles, with the app's theme. Tokens carry a role rather than a colour, so a theme
//! change recolours code without reading it again.

use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::textmate::{Registry, State};

/// What a token is, which the style maps to one of the `--syntax-*` colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SyntaxRole {
    Plain = 0,
    Comment,
    String,
    Number,
    Constant,
    Keyword,
    Operator,
    Function,
    Type,
    Variable,
    Tag,
    Attribute,
    Punctuation,
}

impl SyntaxRole {
    const ALL: [SyntaxRole; 13] = [
        SyntaxRole::Plain,
        SyntaxRole::Comment,
        SyntaxRole::String,
        SyntaxRole::Number,
        SyntaxRole::Constant,
        SyntaxRole::Keyword,
        SyntaxRole::Operator,
        SyntaxRole::Function,
        SyntaxRole::Type,
        SyntaxRole::Variable,
        SyntaxRole::Tag,
        SyntaxRole::Attribute,
        SyntaxRole::Punctuation,
    ];

    fn from_style(style: u8) -> Self {
        Self::ALL.get(style as usize).copied().unwrap_or(SyntaxRole::Plain)
    }
}

/// The app's theme (`THEME` in highlight.ts), rule for rule.
const RULES: [(&str, SyntaxRole); 12] = [
    ("comment, punctuation.definition.comment", SyntaxRole::Comment),
    ("string, string.quoted, string.template, meta.embedded.line", SyntaxRole::String),
    ("constant.numeric, constant.language.boolean, constant.language.null", SyntaxRole::Number),
    ("constant, constant.character.escape, constant.other", SyntaxRole::Constant),
    ("keyword, keyword.control, storage, storage.type, storage.modifier", SyntaxRole::Keyword),
    ("keyword.operator", SyntaxRole::Operator),
    ("entity.name.function, support.function, meta.function-call.generic", SyntaxRole::Function),
    ("entity.name.type, entity.name.class, entity.other.inherited-class, support.type, support.class", SyntaxRole::Type),
    ("variable, variable.parameter, meta.definition.variable", SyntaxRole::Variable),
    ("entity.name.tag", SyntaxRole::Tag),
    ("entity.other.attribute-name, support.type.property-name", SyntaxRole::Attribute),
    ("punctuation, meta.brace", SyntaxRole::Punctuation),
];

/// The grammars highlight.ts loads, with the ones they embed (Shiki 4.6 `@shikijs/langs`).
const GRAMMARS: [&str; 15] = [
    include_str!("../assets/grammars/css.json"),
    include_str!("../assets/grammars/go.json"),
    include_str!("../assets/grammars/html.json"),
    include_str!("../assets/grammars/javascript.json"),
    include_str!("../assets/grammars/json.json"),
    include_str!("../assets/grammars/jsx.json"),
    include_str!("../assets/grammars/markdown.json"),
    include_str!("../assets/grammars/python.json"),
    include_str!("../assets/grammars/rust.json"),
    include_str!("../assets/grammars/shellscript.json"),
    include_str!("../assets/grammars/sql.json"),
    include_str!("../assets/grammars/toml.json"),
    include_str!("../assets/grammars/tsx.json"),
    include_str!("../assets/grammars/typescript.json"),
    include_str!("../assets/grammars/yaml.json"),
];

/// Where a block that is still growing can pick up again: the state before its last line.
struct Resume {
    lang: String,
    /// The code up to the start of its last line, which a longer block must begin with.
    prefix: String,
    state: Option<Arc<State>>,
    tokens: Vec<Token>,
}

const RESUME_SLOTS: usize = 4;

struct Engine {
    registry: Registry,
    resume: Vec<Resume>,
}

fn engine() -> MutexGuard<'static, Engine> {
    static ENGINE: OnceLock<Mutex<Engine>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let rules: Vec<(&str, u8)> = RULES.iter().map(|(selectors, role)| (*selectors, *role as u8)).collect();
            Mutex::new(Engine { registry: Registry::new(&GRAMMARS, &rules), resume: Vec::new() })
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Reads the grammars and compiles the common ones, which takes a moment the first time. Call it
/// off the main thread at start-up so the first coloured block does not wait for it.
pub fn preload() {
    for lang in ["typescript", "tsx", "rust"] {
        let _ = highlight("a", lang);
    }
}

/// Past this a block is not worth a grammar: the pause is longer than the colour is useful.
pub const HIGHLIGHT_LIMIT: usize = 100_000;

/// Whether `lang` names a grammar this build carries.
pub fn has_language(lang: &str) -> bool {
    engine().registry.scope(lang).is_some()
}

/// The grammar a path asks for, as `languageForPath` names it, or `None` for a file drawn plain.
pub fn language_for_path(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let extension = name.rsplit('.').next().unwrap_or(name).to_ascii_lowercase();
    Some(match extension.as_str() {
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "json" => "json",
        "css" => "css",
        "html" => "html",
        "md" | "markdown" => "markdown",
        "sh" | "bash" | "zsh" => "shellscript",
        "py" => "python",
        "rs" => "rust",
        "go" => "go",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "sql" => "sql",
        _ => return None,
    })
}

/// A coloured run: a byte range of the code and its role.
pub type Token = (Range<usize>, SyntaxRole);

fn push_token(tokens: &mut Vec<Token>, range: Range<usize>, role: SyntaxRole) {
    if range.is_empty() {
        return;
    }
    match tokens.last_mut() {
        Some((last, last_role)) if *last_role == role && last.end == range.start => last.end = range.end,
        _ => tokens.push((range, role)),
    }
}

/// A block of code as coloured tokens over its bytes, line breaks left out. Whole blocks rather
/// than single lines, because a line read alone has lost whatever string or comment opened above
/// it. A block that grows from one asked for before resumes from that block's last line.
pub fn highlight(code: &str, lang: &str) -> Option<Vec<Token>> {
    if code.is_empty() || code.len() > HIGHLIGHT_LIMIT {
        return None;
    }
    let mut engine = engine();
    let engine = &mut *engine;
    let scope = engine.registry.scope(lang)?.to_string();
    let resumed = engine
        .resume
        .iter()
        .position(|slot| slot.lang == lang && code.starts_with(&slot.prefix))
        .map(|index| engine.resume.remove(index));
    let (mut state, mut tokens, mut offset) = match resumed {
        Some(resume) => {
            let offset = resume.prefix.len();
            (resume.state, resume.tokens, offset)
        }
        None => (None, Vec::new(), 0),
    };
    let mut last_line;
    loop {
        let (end, next) = match code[offset..].find('\n') {
            Some(at) => (offset + at, Some(offset + at + 1)),
            None => (code.len(), None),
        };
        last_line = (offset, state.clone(), tokens.len());
        let line = &code[offset..end];
        let line = line.strip_suffix('\r').unwrap_or(line);
        if !line.is_empty() {
            let (line_tokens, next_state) = engine.registry.tokenize_line(&scope, line, state.as_ref())?;
            for (index, (start, style)) in line_tokens.iter().enumerate() {
                let stop = line_tokens.get(index + 1).map_or(line.len(), |(next, _)| *next).min(line.len());
                push_token(&mut tokens, offset + start..offset + stop, SyntaxRole::from_style(*style));
            }
            state = Some(next_state);
        }
        match next {
            Some(next) => offset = next,
            None => break,
        }
    }
    let (prefix_end, resume_state, kept) = last_line;
    let mut resume_tokens: Vec<Token> = tokens[..kept].to_vec();
    if let Some((range, _)) = resume_tokens.last_mut() {
        range.end = range.end.min(prefix_end);
    }
    if engine.resume.len() >= RESUME_SLOTS {
        engine.resume.remove(0);
    }
    engine.resume.push(Resume { lang: lang.to_string(), prefix: code[..prefix_end].to_string(), state: resume_state, tokens: resume_tokens });
    Some(tokens)
}

/// The same tokens split per line, with ranges relative to each line. Lines are split at `\n`.
pub fn highlight_lines(code: &str, lang: &str) -> Option<Vec<Vec<Token>>> {
    let tokens = highlight(code, lang)?;
    let mut lines = Vec::new();
    let mut start = 0;
    let mut cursor = 0;
    for line in code.split('\n') {
        let end = start + line.len();
        let mut row = Vec::new();
        while cursor < tokens.len() && tokens[cursor].0.start < end {
            let (range, role) = &tokens[cursor];
            let clipped = range.start.max(start)..range.end.min(end);
            if !clipped.is_empty() {
                row.push((clipped.start - start..clipped.end - start, *role));
            }
            if range.end <= end + 1 {
                cursor += 1;
            } else {
                break;
            }
        }
        lines.push(row);
        start = end + 1;
    }
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_growing_block_resumes_with_the_same_tokens() {
        let first = "fn main() {\n    let s = \"a\n";
        let whole = format!("{first}b\";\n}}\n");
        let _ = highlight(first, "rust");
        let resumed = highlight(&whole, "rust").unwrap();
        engine().resume.clear();
        assert_eq!(resumed, highlight(&whole, "rust").unwrap());
    }

    /// Each byte's role as Shiki 4.6 colours it with the app theme (one letter per role).
    fn roles(code: &str, lang: &str) -> String {
        let mut out: Vec<u8> = code.bytes().map(|byte| if byte == b'\n' { b'\n' } else { b'.' }).collect();
        for (range, role) in highlight(code, lang).unwrap() {
            let letter = b".CSNKWOFTVGAP"[role as usize];
            out[range].fill(letter);
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn colours_match_shiki_token_for_token() {
        let cases = [
            ("toml", "a = { b = \"x\", c = [1, true] }\n", "V.P.P.V.P.PSPP.V.P.PNP.NNNNP.P\n"),
            (
                "markdown",
                "# T\n\n```ts\nconst a: number = 1 // c\n```\n> quote *em*\n",
                "P..\n\nPPP..\nWWWWW.VO.TTTTTT.O.N.CCCC\nPPP\nP.......P..P\n",
            ),
            (
                "html",
                "<style>a { color: red }</style>\n<script>let x = `t${1}`</script>\n<!-- c --><p class=\"k\">&amp;</p>\n",
                "PGGGGGP..P.AAAAAP.....PPPGGGGGP\nPGGGGGGPWWW.V.O.PSPPNPPOOVVVVVVO\nOOOO.V.OOOPG.AAAAAOPSPPPKKKPPPGP\n",
            ),
            (
                "shellscript",
                "cat <<EOF\nhi $USER\nEOF\necho \"${x:-1}\" | grep -E '^a'\n",
                "FFF.OOPPP\nSSSVVVVV\nPPP\nFFFF.PPPVOOVPP.O.FFFF.KK.PSSP\n",
            ),
            (
                "python",
                "x = f\"{a!r:>10} {b}\"\n\n\"\"\"doc\n\nstring\"\"\"\ndef g(*args): return 0x1F\n",
                "..O.WSK.WWWWWWKSK.KS\n\nPPPSSS\n\nSSSSSSPPP\nWWW.FPOVVVVPP.WWWWWW.WWNN\n",
            ),
            (
                "tsx",
                "const r = /a+[b]/gi;\n/*\n\n*/ let s = <div onClick={() => f(1)}>t</div>\n",
                "WWWWW.V.OSPSOPKPPWWP\nCC\n\nCC.WWW.V.O.PGGG.AAAAAAAOPPP.WW.FPNPPP.PPGGGP\n",
            ),
        ];
        for (lang, code, expected) in cases {
            assert_eq!(roles(code, lang), expected, "{lang}");
        }
    }

    #[test]
    fn unknown_languages_draw_plain() {
        assert!(highlight("x", "no-such-language").is_none());
        assert!(highlight("let x = 1", "swift").is_none());
        assert_eq!(language_for_path("src/a.test.MTS"), Some("typescript"));
        assert!(has_language("shellscript") && has_language("tsx") && has_language("toml"));
    }
}
