//! Works out, from a shell command as written, the files it reads, edits, or
//! writes, and the folders and patterns it takes files in by, as far as the
//! command shows. Anything it can't work out, such as a path built from a
//! variable or a command substitution, is left out rather than guessed at.

use std::path::{Component, Path, PathBuf};

/// What a shell command works on.
#[derive(Debug, Default, PartialEq)]
pub struct ShellPaths {
    /// Each file it names, and whether it edits or writes it, in order.
    pub files: Vec<(PathBuf, bool)>,
    /// The folders it takes files in by, named or under a pattern. A file in
    /// one counts only when the command's output names it.
    pub scopes: Vec<Scope>,
}

/// A folder a command or search takes files in by, and the folder the paths
/// its output names are read from.
#[derive(Clone, Debug, PartialEq)]
pub struct Scope {
    pub dir: PathBuf,
    pub base: PathBuf,
}

impl Scope {
    /// The files in the scope that a line of output names: a line that is a
    /// path, one that starts with a path and a colon, as a search's matches
    /// do, or a heading naming one, as `head`, `tail`, and `diff` print.
    pub fn named_in(&self, line: &str) -> Option<PathBuf> {
        let line = line.trim();
        let heading = line
            .strip_prefix("==> ")
            .and_then(|rest| rest.strip_suffix(" <=="))
            .or_else(|| line.strip_prefix("+++ b/"))
            .or_else(|| line.strip_prefix("--- a/"))
            .or_else(|| line.strip_prefix("+++ "))
            .or_else(|| line.strip_prefix("--- "))
            .map(|rest| rest.split('\t').next().unwrap_or(rest));
        let before_colon = line.split_once(':').map(|(path, _)| path);
        [heading, Some(line), before_colon]
            .into_iter()
            .flatten()
            .filter(|path| !path.is_empty())
            .map(|path| normalize(&self.base.join(path)))
            .find(|path| path.starts_with(&self.dir) && path.is_file())
    }
}

/// The files and scopes `command` works on, its relative paths read from
/// `dir`.
pub fn analyze(command: &str, dir: &Path) -> ShellPaths {
    let mut paths = ShellPaths::default();
    let mut cwd = Some(dir.to_path_buf());
    let tokens = tokens(command);
    for segment in tokens.split(|token| *token == Token::Break) {
        simple_command(segment, &mut cwd, &mut paths);
    }
    paths
}

/// A path with its `.` and `..` parts worked out, without touching the
/// file system.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other),
        }
    }
    out
}

/// A word of a command as the shell would read it.
#[derive(Clone, Debug, PartialEq)]
struct Word {
    text: String,
    /// Whether it can be worked out without running anything: no variable,
    /// command substitution, or home directory in it.
    known: bool,
    /// Whether it has a pattern in it, for the shell to expand.
    pattern: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Word(Word),
    /// Ends a simple command: `;`, `&`, `&&`, `||`, `|`, a parenthesis, or a
    /// line break.
    Break,
    /// A redirect, whose target is the next word.
    Redirect(Redirect),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Redirect {
    Read,
    Write,
    /// A duplicated descriptor or a here-string, whose word is no file.
    NotAFile,
}

fn tokens(command: &str) -> Vec<Token> {
    let chars: Vec<char> = command.chars().collect();
    let mut tokens = Vec::new();
    // Here-documents opened on the current line, whose bodies follow it.
    let mut heredocs: Vec<(String, bool)> = Vec::new();
    let mut delimiter_next: Option<bool> = None;
    let mut i = 0;
    while i < chars.len() {
        let next = chars.get(i + 1).copied();
        match chars[i] {
            ' ' | '\t' => i += 1,
            '\n' => {
                i += 1;
                tokens.push(Token::Break);
                for (delimiter, strip_tabs) in heredocs.drain(..) {
                    while i < chars.len() {
                        let end = chars[i..]
                            .iter()
                            .position(|c| *c == '\n')
                            .map_or(chars.len(), |n| i + n);
                        let line: String = chars[i..end].iter().collect();
                        i = (end + 1).min(chars.len());
                        let line = if strip_tabs {
                            line.trim_start_matches('\t')
                        } else {
                            &line
                        };
                        if line == delimiter {
                            break;
                        }
                    }
                }
            }
            '#' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            ';' | '(' | ')' => {
                i += 1;
                tokens.push(Token::Break);
            }
            '|' => {
                i += if next == Some('|') { 2 } else { 1 };
                tokens.push(Token::Break);
            }
            '&' if next == Some('>') => {
                i += if chars.get(i + 2) == Some(&'>') { 3 } else { 2 };
                tokens.push(Token::Redirect(Redirect::Write));
            }
            '&' => {
                i += if next == Some('&') { 2 } else { 1 };
                tokens.push(Token::Break);
            }
            '<' if chars[i..].starts_with(&['<', '<', '<']) => {
                i += 3;
                tokens.push(Token::Redirect(Redirect::NotAFile));
            }
            '<' if next == Some('<') => {
                let strip_tabs = chars.get(i + 2) == Some(&'-');
                i += if strip_tabs { 3 } else { 2 };
                delimiter_next = Some(strip_tabs);
            }
            '<' | '>' if next == Some('&') => {
                i += 2;
                tokens.push(Token::Redirect(Redirect::NotAFile));
            }
            '<' => {
                i += if next == Some('>') { 2 } else { 1 };
                tokens.push(Token::Redirect(Redirect::Read));
            }
            '>' => {
                i += if matches!(next, Some('>' | '|')) {
                    2
                } else {
                    1
                };
                tokens.push(Token::Redirect(Redirect::Write));
            }
            _ => {
                // A descriptor's number before a redirect belongs to it.
                let digits = chars[i..].iter().take_while(|c| c.is_ascii_digit()).count();
                if digits > 0 && matches!(chars.get(i + digits), Some('<' | '>')) {
                    i += digits;
                    continue;
                }
                let (word, end) = word(&chars, i);
                i = end;
                match delimiter_next.take() {
                    Some(strip_tabs) => heredocs.push((word.text, strip_tabs)),
                    None => tokens.push(Token::Word(word)),
                }
            }
        }
    }
    tokens
}

/// The word starting at `start`, and where it ends.
fn word(chars: &[char], start: usize) -> (Word, usize) {
    let mut word = Word {
        text: String::new(),
        known: true,
        pattern: false,
    };
    let mut i = start;
    while let Some(&c) = chars.get(i) {
        match c {
            ' ' | '\t' | '\n' | ';' | '&' | '|' | '<' | '>' | '(' | ')' => break,
            '\'' => {
                i += 1;
                while let Some(&c) = chars.get(i) {
                    i += 1;
                    if c == '\'' {
                        break;
                    }
                    word.text.push(c);
                }
            }
            '"' => {
                i += 1;
                while let Some(&c) = chars.get(i) {
                    match c {
                        '"' => {
                            i += 1;
                            break;
                        }
                        '\\' if matches!(chars.get(i + 1), Some('"' | '\\' | '$' | '`')) => {
                            word.text.push(chars[i + 1]);
                            i += 2;
                        }
                        '$' | '`' => {
                            word.known = false;
                            i = substitution(chars, i);
                        }
                        _ => {
                            word.text.push(c);
                            i += 1;
                        }
                    }
                }
            }
            '\\' => {
                if let Some(&escaped) = chars.get(i + 1)
                    && escaped != '\n'
                {
                    word.text.push(escaped);
                }
                i += 2;
            }
            '$' | '`' => {
                word.known = false;
                i = substitution(chars, i);
            }
            '~' if i == start => {
                word.known = false;
                i += 1;
            }
            '*' | '?' | '[' | '{' => {
                word.pattern = true;
                word.text.push(c);
                i += 1;
            }
            _ => {
                word.text.push(c);
                i += 1;
            }
        }
    }
    (word, i)
}

/// Where a variable or command substitution starting at `start` ends.
fn substitution(chars: &[char], start: usize) -> usize {
    let closing = |open: char, close: char, from: usize| {
        let mut depth = 0;
        let mut i = from;
        while let Some(&c) = chars.get(i) {
            i += 1;
            if c == open {
                depth += 1;
            } else if c == close {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
        }
        i
    };
    if chars[start] == '`' {
        return chars[start + 1..]
            .iter()
            .position(|c| *c == '`')
            .map_or(chars.len(), |n| start + n + 2);
    }
    match chars.get(start + 1) {
        Some('(') => closing('(', ')', start + 1),
        Some('{') => closing('{', '}', start + 1),
        Some('\'') => chars[start + 2..]
            .iter()
            .position(|c| *c == '\'')
            .map_or(chars.len(), |n| start + n + 3),
        Some(c) if c.is_alphanumeric() || c == &'_' => {
            start
                + 1
                + chars[start + 1..]
                    .iter()
                    .take_while(|c| c.is_alphanumeric() || **c == '_')
                    .count()
        }
        Some(_) => start + 2,
        None => start + 1,
    }
}

/// What a simple command, one between breaks, works on, following `cd` for
/// those after it.
fn simple_command(tokens: &[Token], cwd: &mut Option<PathBuf>, paths: &mut ShellPaths) {
    let mut args = Vec::new();
    let mut tokens = tokens.iter();
    while let Some(token) = tokens.next() {
        match token {
            Token::Word(word) => args.push(word.clone()),
            Token::Redirect(redirect) => {
                let Some(Token::Word(target)) = tokens.next() else {
                    continue;
                };
                let Some(path) = resolve(cwd, target).filter(|path| !path.starts_with("/dev"))
                else {
                    continue;
                };
                match redirect {
                    Redirect::Write => paths.files.push((path, true)),
                    Redirect::Read if path.is_file() => paths.files.push((path, false)),
                    _ => {}
                }
            }
            Token::Break => {}
        }
    }
    // Assignments and commands that only run the next one come first.
    let start = args
        .iter()
        .position(|arg| {
            let assignment = arg.text.split_once('=').is_some_and(|(name, _)| {
                !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
            });
            let wrapper = matches!(
                arg.text.as_str(),
                "sudo" | "env" | "command" | "builtin" | "exec" | "time" | "nice" | "nohup"
            );
            !assignment && !wrapper
        })
        .unwrap_or(args.len());
    let args = &args[start..];
    let Some(program) = args.first().filter(|program| program.known) else {
        return;
    };
    let program = program.text.rsplit('/').next().unwrap_or_default();
    let args = &args[1..];
    let mut work = Work { cwd, paths };
    match program {
        "cd" | "pushd" => {
            let parsed = parse(args, &[]);
            *work.cwd = match parsed.operands.first() {
                Some(dir) if dir.text != "-" => resolve(work.cwd, dir),
                _ => None,
            };
        }
        "cat" | "less" | "more" | "bat" | "batcat" | "nl" | "wc" | "tac" | "cmp" | "od" | "xxd"
        | "hexdump" | "strings" | "file" | "stat" | "md5sum" | "sha1sum" | "sha256sum"
        | "column" | "fold" | "fmt" | "rev" | "paste" => {
            work.read_all(&parse(args, &[]).operands, false);
        }
        "head" | "tail" => work.read_all(&parse(args, &["-n", "-c"]).operands, false),
        "cut" => work.read_all(&parse(args, &["-d", "-f", "-c", "-b"]).operands, false),
        "diff" => {
            let parsed = parse(args, &["-U", "-C", "-L", "-x", "-X", "-I"]);
            work.read_all(&parsed.operands, true);
        }
        "sort" => {
            let parsed = parse(args, &["-k", "-t", "-o", "-S", "-T"]);
            work.read_all(&parsed.operands, false);
            work.write(parsed.value("-o"));
        }
        "grep" | "egrep" | "fgrep" => {
            let parsed = parse(
                args,
                &[
                    "-e", "--regexp", "-f", "--file", "-m", "-A", "-B", "-C", "-d", "-D",
                ],
            );
            let recursive =
                parsed.has_flag('r') || parsed.has_flag('R') || parsed.has("--recursive");
            let operands = parsed.after_pattern(&["-e", "--regexp", "-f", "--file"]);
            work.search(operands, recursive);
        }
        "rg" | "ag" => {
            let parsed = parse(
                args,
                &[
                    "-e",
                    "--regexp",
                    "-f",
                    "--file",
                    "-g",
                    "--glob",
                    "--iglob",
                    "-t",
                    "--type",
                    "-T",
                    "--type-not",
                    "-m",
                    "-A",
                    "-B",
                    "-C",
                    "-M",
                    "-j",
                    "--max-depth",
                ],
            );
            let operands = parsed.after_pattern(&["-e", "--regexp", "-f", "--file"]);
            work.search(operands, true);
        }
        "find" => {
            let starts: Vec<&Word> = args
                .iter()
                .take_while(|arg| !arg.text.starts_with(['-', '(', '!']))
                .collect();
            work.search(&starts, true);
        }
        "sed" => {
            let parsed = parse(args, &["-e", "--expression", "-f", "--file", "-l"]);
            let edits = parsed.options.iter().any(|(option, _)| {
                option.starts_with("--in-place")
                    || (!option.starts_with("--") && option.contains('i'))
            });
            let operands = parsed.after_pattern(&["-e", "--expression", "-f", "--file"]);
            work.read_or_edit(operands, edits);
        }
        "awk" | "gawk" => {
            let parsed = parse(args, &["-f", "--file", "-v", "-F", "-i"]);
            let edits = parsed
                .value("-i")
                .is_some_and(|value| value.text == "inplace");
            let operands = parsed.after_pattern(&["-f", "--file"]);
            work.read_or_edit(operands, edits);
        }
        "perl" => {
            let parsed = parse(args, &["-e", "-E", "-I", "-M"]);
            let edits = parsed
                .options
                .iter()
                .any(|(option, _)| !option.starts_with("--") && option.contains('i'));
            let operands = parsed.after_pattern(&["-e", "-E"]);
            if edits {
                work.read_or_edit(operands, true);
            }
        }
        "jq" | "yq" => {
            let parsed = parse(args, &["--arg", "--argjson", "-f", "--from-file"]);
            work.read_all(parsed.after_pattern(&["-f", "--from-file"]), false);
        }
        "tee" => {
            for operand in parse(args, &[]).operands {
                work.write(Some(operand));
            }
        }
        "cp" | "mv" => {
            let parsed = parse(args, &["-t", "--target-directory", "-S", "--suffix"]);
            work.copy(&parsed);
        }
        "git" => {
            let Some(subcommand) = args.first() else {
                return;
            };
            if matches!(subcommand.text.as_str(), "diff" | "show" | "log" | "blame") {
                let parsed = parse(&args[1..], &["-U", "-n", "--format", "--pretty"]);
                work.read_all(&parsed.operands, true);
                if parsed.operands.is_empty()
                    && let Some(dir) = work.cwd.clone()
                {
                    work.scope(dir);
                }
            }
        }
        _ => {}
    }
}

/// A command's words split into options, each with any value it takes, and
/// operands.
struct Parsed<'a> {
    options: Vec<(&'a str, Option<&'a Word>)>,
    operands: Vec<&'a Word>,
}

/// Splits `args`, `takes_value` being the options whose value is the next
/// word.
fn parse<'a>(args: &'a [Word], takes_value: &[&str]) -> Parsed<'a> {
    let mut parsed = Parsed {
        options: Vec::new(),
        operands: Vec::new(),
    };
    let mut args = args.iter();
    let mut options_over = false;
    while let Some(arg) = args.next() {
        let text = arg.text.as_str();
        if options_over || !text.starts_with('-') || text == "-" {
            parsed.operands.push(arg);
        } else if text == "--" {
            options_over = true;
        } else if takes_value.contains(&text) {
            parsed.options.push((text, args.next()));
        } else {
            parsed.options.push((text, None));
        }
    }
    parsed
}

impl<'a> Parsed<'a> {
    fn value(&self, option: &str) -> Option<&'a Word> {
        self.options
            .iter()
            .find(|(name, _)| *name == option)
            .and_then(|(_, value)| *value)
    }

    fn has(&self, option: &str) -> bool {
        self.options.iter().any(|(name, _)| *name == option)
    }

    /// Whether a one-letter flag is given, alone or among others.
    fn has_flag(&self, flag: char) -> bool {
        self.options
            .iter()
            .any(|(name, _)| !name.starts_with("--") && name.contains(flag))
    }

    /// The operands after the pattern or script that comes first, unless one
    /// of `given_by` gave it instead.
    fn after_pattern(&self, given_by: &[&str]) -> &[&'a Word] {
        let given = self.options.iter().any(|(name, _)| {
            given_by.contains(name)
                || given_by
                    .iter()
                    .any(|long| name.starts_with(&format!("{long}=")))
        });
        if given {
            &self.operands
        } else {
            self.operands.get(1..).unwrap_or_default()
        }
    }
}

/// A command's work as it is worked out.
struct Work<'a> {
    cwd: &'a mut Option<PathBuf>,
    paths: &'a mut ShellPaths,
}

impl Work<'_> {
    /// Operands read whole: a file named is read; a folder or pattern is a
    /// scope when `folders` says the command takes files in by them.
    fn read_all(&mut self, operands: &[&Word], folders: bool) {
        for operand in operands {
            if operand.pattern {
                if let Some(dir) = pattern_dir(self.cwd, operand) {
                    self.scope(dir);
                }
                continue;
            }
            let Some(path) = resolve(self.cwd, operand) else {
                continue;
            };
            if path.is_file() {
                self.paths.files.push((path, false));
            } else if folders && path.is_dir() {
                self.scope(path);
            }
        }
    }

    /// A search's operands: files are read, folders searched when it
    /// recurses, and with none it searches the folder it's in.
    fn search(&mut self, operands: &[&Word], recursive: bool) {
        if operands.is_empty() {
            if recursive && let Some(dir) = self.cwd.clone() {
                self.scope(dir);
            }
            return;
        }
        self.read_all(operands, recursive);
    }

    fn read_or_edit(&mut self, operands: &[&Word], edits: bool) {
        if !edits {
            return self.read_all(operands, false);
        }
        for operand in operands {
            if let Some(path) = resolve(self.cwd, operand).filter(|path| path.is_file()) {
                self.paths.files.push((path, true));
            }
        }
    }

    fn write(&mut self, target: Option<&Word>) {
        if let Some(path) = target
            .and_then(|target| resolve(self.cwd, target))
            .filter(|path| !path.starts_with("/dev"))
        {
            self.paths.files.push((path, true));
        }
    }

    /// `cp` or `mv`: sources read, and what they're copied or moved onto
    /// written.
    fn copy(&mut self, parsed: &Parsed) {
        let (sources, target_dir) = match parsed
            .value("-t")
            .or_else(|| parsed.value("--target-directory"))
        {
            Some(dir) => (&parsed.operands[..], Some(dir)),
            None => match parsed.operands.split_last() {
                Some((target, sources)) if !sources.is_empty() => {
                    let into_dir = sources.len() > 1
                        || target.text.ends_with('/')
                        || resolve(self.cwd, target).is_some_and(|path| path.is_dir());
                    if !into_dir {
                        let source = sources[0];
                        if let Some(path) = resolve(self.cwd, source).filter(|path| path.is_file())
                        {
                            self.paths.files.push((path, false));
                        }
                        return self.write(Some(*target));
                    }
                    (sources, Some(*target))
                }
                _ => return,
            },
        };
        let Some(dir) = target_dir.and_then(|dir| resolve(self.cwd, dir)) else {
            return;
        };
        for source in sources {
            let Some(path) = resolve(self.cwd, source).filter(|path| path.is_file()) else {
                continue;
            };
            let Some(name) = path.file_name() else {
                continue;
            };
            let written = dir.join(name);
            self.paths.files.push((path, false));
            self.paths.files.push((written, true));
        }
    }

    fn scope(&mut self, dir: PathBuf) {
        if let Some(base) = self.cwd.clone() {
            self.paths.scopes.push(Scope { dir, base });
        }
    }
}

/// The path a word names, if it can be worked out.
fn resolve(cwd: &Option<PathBuf>, word: &Word) -> Option<PathBuf> {
    if !word.known || word.pattern || word.text.is_empty() {
        return None;
    }
    let path = Path::new(&word.text);
    if path.is_absolute() {
        return Some(normalize(path));
    }
    Some(normalize(&cwd.as_ref()?.join(path)))
}

/// The folder a pattern's matches are all in: the part of it before its first
/// wildcard, up to the last `/` there.
fn pattern_dir(cwd: &Option<PathBuf>, word: &Word) -> Option<PathBuf> {
    if !word.known {
        return None;
    }
    let literal = &word.text[..word
        .text
        .find(['*', '?', '[', '{'])
        .unwrap_or(word.text.len())];
    let dir = &literal[..literal.rfind('/').map_or(0, |n| n + 1)];
    let path = Path::new(dir);
    if path.is_absolute() {
        return Some(normalize(path));
    }
    Some(normalize(&cwd.as_ref()?.join(path)))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Scope, analyze, normalize};

    /// A project with `spec/a.pi`, `spec/b/c.pi`, and `src/main.rs`.
    fn project(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("suspense-shell-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("spec/b")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        for file in ["spec/a.pi", "spec/b/c.pi", "src/main.rs"] {
            std::fs::write(dir.join(file), "x\n").unwrap();
        }
        dir
    }

    fn files(command: &str, dir: &Path) -> Vec<(String, bool)> {
        analyze(command, dir)
            .files
            .into_iter()
            .map(|(path, edited)| {
                let shown = path
                    .strip_prefix(dir)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                (shown, edited)
            })
            .collect()
    }

    fn read(path: &str) -> (String, bool) {
        (path.to_string(), false)
    }

    fn written(path: &str) -> (String, bool) {
        (path.to_string(), true)
    }

    #[test]
    fn reads_the_files_commands_name() {
        let dir = project("reads");
        assert_eq!(files("cat spec/a.pi", &dir), [read("spec/a.pi")]);
        assert_eq!(
            files("head -n 5 spec/a.pi | tail -2", &dir),
            [read("spec/a.pi")]
        );
        assert_eq!(files("sed -n '1,5p' spec/a.pi", &dir), [read("spec/a.pi")]);
        assert_eq!(
            files("grep -n foo spec/a.pi src/main.rs", &dir),
            [read("spec/a.pi"), read("src/main.rs")]
        );
        assert_eq!(files("cd spec && less b/c.pi", &dir), [read("spec/b/c.pi")]);
        assert_eq!(
            files("diff spec/a.pi ./spec/b/../a.pi", &dir),
            [read("spec/a.pi"), read("spec/a.pi")]
        );
        assert_eq!(
            files("awk '{print}' < spec/a.pi", &dir),
            [read("spec/a.pi")]
        );
        let absolute = dir.join("spec/a.pi").display().to_string();
        assert_eq!(
            files(&format!("rg x \"{absolute}\""), &dir),
            [read("spec/a.pi")]
        );
    }

    #[test]
    fn edits_and_writes() {
        let dir = project("writes");
        assert_eq!(
            files("sed -i 's/x/y/' spec/a.pi", &dir),
            [written("spec/a.pi")]
        );
        assert_eq!(
            files("echo x >> spec/new.pi", &dir),
            [written("spec/new.pi")]
        );
        assert_eq!(
            files("printf x | tee -a spec/a.pi > /dev/null", &dir),
            [written("spec/a.pi")]
        );
        assert_eq!(
            files(
                "cat > spec/new.pi <<'EOF'\ncat spec/a.pi\nEOF\ncat src/main.rs",
                &dir
            ),
            [written("spec/new.pi"), read("src/main.rs")]
        );
        assert_eq!(
            files("cp src/main.rs spec/b", &dir),
            [read("src/main.rs"), written("spec/b/main.rs")]
        );
        assert_eq!(
            files("mv spec/a.pi spec/d.pi 2>&1", &dir),
            [read("spec/a.pi"), written("spec/d.pi")]
        );
    }

    /// A target built from a variable or a substitution, or a pattern, isn't
    /// guessed at; nor is a word that only looks like a file.
    #[test]
    fn skips_what_it_cant_work_out() {
        let dir = project("skips");
        assert!(files("cat $FILE spec/$(ls spec | head -1)", &dir).is_empty());
        assert!(files("cd \"$DIR\" && cat a.pi", &dir).is_empty());
        assert!(files("cat spec/*.pi", &dir).is_empty());
        assert_eq!(
            files("grep spec/a.pi src/main.rs", &dir),
            [read("src/main.rs")]
        );
        assert!(files("echo 'cat spec/a.pi' # cat spec/a.pi", &dir).is_empty());
    }

    /// A folder or pattern a command takes files in by is a scope, whose
    /// files count when the output names them.
    #[test]
    fn scopes_name_files_in_the_output() {
        let dir = project("scopes");
        let scopes = analyze("grep -rn foo spec", &dir).scopes;
        assert_eq!(
            scopes,
            [Scope {
                dir: dir.join("spec"),
                base: dir.clone()
            }]
        );
        let scope = &scopes[0];
        assert_eq!(
            scope.named_in("spec/b/c.pi:3:foo"),
            Some(dir.join("spec/b/c.pi"))
        );
        assert_eq!(
            scope.named_in("==> spec/a.pi <=="),
            Some(dir.join("spec/a.pi"))
        );
        assert_eq!(scope.named_in("src/main.rs:1:foo"), None);
        assert_eq!(scope.named_in("spec/missing.pi"), None);
        assert_eq!(
            analyze("cat spec/*.pi", &dir).scopes[0].dir,
            dir.join("spec")
        );
        assert_eq!(analyze("rg foo", &dir).scopes[0].dir, dir);
        assert!(analyze("grep foo spec", &dir).scopes.is_empty());
    }

    #[test]
    fn normalizes_lexically() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), Path::new("/a/c"));
        assert_eq!(normalize(Path::new("/../a")), Path::new("/a"));
        assert_eq!(normalize(Path::new("../a")), Path::new("../a"));
    }
}
