use super::fs_util::display_path;
use std::fs;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Sync,
    Ignore,
    MetadataOnly,
    LocalOnly,
    Secret,
    HydrateOnAccess,
}

impl Action {
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "sync" => Some(Self::Sync),
            "ignore" => Some(Self::Ignore),
            "metadata-only" => Some(Self::MetadataOnly),
            "local-only" => Some(Self::LocalOnly),
            "secret" => Some(Self::Secret),
            "hydrate-on-access" => Some(Self::HydrateOnAccess),
            _ => None,
        }
    }

    pub fn state_label(self) -> &'static str {
        match self {
            Self::Sync => "local",
            Self::Ignore => "ignored",
            Self::MetadataOnly => "metadata-only",
            Self::LocalOnly => "local-only",
            Self::Secret => "secret-locked",
            Self::HydrateOnAccess => "remote-only",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Ignore => "ignore",
            Self::MetadataOnly => "metadata-only",
            Self::LocalOnly => "local-only",
            Self::Secret => "secret",
            Self::HydrateOnAccess => "hydrate-on-access",
        }
    }

    pub fn skips_children(self) -> bool {
        matches!(self, Self::Ignore | Self::MetadataOnly | Self::LocalOnly)
    }
}

#[derive(Debug)]
pub struct Rule {
    pub pattern: String,
    pub action: Action,
    pub dir_only: bool,
}

impl Rule {
    pub fn new(pattern: &str, action: Action) -> Self {
        let dir_only = pattern.ends_with('/');
        Self {
            pattern: pattern
                .trim_start_matches('/')
                .trim_end_matches('/')
                .to_string(),
            action,
            dir_only,
        }
    }

    pub fn parse(line: &str) -> Option<Self> {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            return None;
        }

        let mut fields = line.split_whitespace();
        let mut pattern = fields.next()?;
        let mut action = Action::Ignore;

        if let Some(stripped) = pattern.strip_prefix('!') {
            pattern = stripped;
            action = Action::Sync;
        }

        if let Some(token) = fields.next().and_then(Action::from_token) {
            action = token;
        }

        (!pattern.is_empty()).then(|| Self::new(pattern, action))
    }

    pub fn matches(&self, rel: &str, is_dir: bool) -> bool {
        let rel = rel.trim_start_matches("./");
        if rel.is_empty() {
            return false;
        }

        if self.dir_only {
            return self.matches_directory(rel, is_dir);
        }

        if self.pattern.contains('/') {
            wildcard_match(&self.pattern, rel)
        } else {
            rel.rsplit('/')
                .next()
                .is_some_and(|name| wildcard_match(&self.pattern, name))
        }
    }

    pub fn matches_directory(&self, rel: &str, is_dir: bool) -> bool {
        if self.pattern.contains('/') {
            return rel == self.pattern
                || rel
                    .strip_prefix(&self.pattern)
                    .is_some_and(|rest| rest.starts_with('/'));
        }

        let mut parts = rel.split('/').peekable();
        while let Some(part) = parts.next() {
            if wildcard_match(&self.pattern, part) && (is_dir || parts.peek().is_some()) {
                return true;
            }
        }

        false
    }
}

pub struct Rules {
    pub rules: Vec<Rule>,
    pub default_count: usize,
    pub custom_count: usize,
}

impl Rules {
    pub fn load(root: &Path) -> Result<Self, String> {
        let mut rules = default_rules();
        let default_count = rules.len();
        let mut custom_count = 0;
        let custom_path = root.join(".devsyncignore");

        if custom_path.exists() {
            let text = fs::read_to_string(&custom_path)
                .map_err(|err| format!("read {}: {err}", display_path(&custom_path)))?;
            for line in text.lines() {
                if let Some(rule) = Rule::parse(line) {
                    rules.push(rule);
                    custom_count += 1;
                }
            }
        }

        Ok(Self {
            rules,
            default_count,
            custom_count,
        })
    }

    pub fn action_for(&self, rel: &str, is_dir: bool) -> Action {
        self.rules
            .iter()
            .rev()
            .find(|rule| rule.matches(rel, is_dir))
            .map(|rule| rule.action)
            .unwrap_or(Action::Sync)
    }
}

pub fn default_rules() -> Vec<Rule> {
    [
        (".git/", Action::LocalOnly),
        (".devdrop/", Action::LocalOnly),
        ("node_modules/", Action::MetadataOnly),
        (".pnpm-store/", Action::LocalOnly),
        (".venv/", Action::LocalOnly),
        ("venv/", Action::LocalOnly),
        ("vendor/", Action::MetadataOnly),
        (".next/", Action::Ignore),
        (".nuxt/", Action::Ignore),
        (".turbo/", Action::Ignore),
        (".vite/", Action::Ignore),
        ("dist/", Action::Ignore),
        ("build/", Action::Ignore),
        ("target/", Action::LocalOnly),
        (".cache/", Action::Ignore),
        ("__pycache__/", Action::Ignore),
        (".pytest_cache/", Action::Ignore),
        (".mypy_cache/", Action::Ignore),
        (".gradle/", Action::LocalOnly),
        ("DerivedData/", Action::LocalOnly),
        (".build/", Action::LocalOnly),
        ("tmp/", Action::Ignore),
        ("*.pyc", Action::Ignore),
        ("*.log", Action::Ignore),
        (".DS_Store", Action::Ignore),
        (".env", Action::Secret),
        (".env.*", Action::Secret),
        ("*.pem", Action::Secret),
        ("*.key", Action::Secret),
        ("id_rsa", Action::Secret),
        ("id_ed25519", Action::Secret),
        (".env.example", Action::Sync),
    ]
    .into_iter()
    .map(|(pattern, action)| Rule::new(pattern, action))
    .collect()
}

pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut p, mut t) = (0, 0);
    let mut star = None;
    let mut star_text = 0;

    while t < text.len() {
        if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            star_text = t;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            star_text += 1;
            t = star_text;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}
