impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            tokens: tokenize(source),
            position: 0,
            _source: source,
        }
    }
    fn parse(mut self) -> Result<Value, String> {
        self.value()
    }
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }
    fn take(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.position).cloned();
        self.position += 1;
        token
    }
    fn value(&mut self) -> Result<Value, String> {
        match self.take() {
            Some(Token::At) => match self.take() {
                Some(Token::OpenBrace) => self.object(),
                Some(Token::OpenParen) => self.array(),
                other => Err(format!(
                    "expected hashtable or array after @, got {other:?}"
                )),
            },
            Some(Token::OpenParen) => self.array(),
            Some(Token::Word(value)) => Ok(match value.as_str() {
                "$true" => Value::Bool(true),
                "$false" => Value::Bool(false),
                "$null" => Value::Null,
                _ => Value::String(value),
            }),
            other => Err(format!("unexpected token {other:?}")),
        }
    }
    fn object(&mut self) -> Result<Value, String> {
        let mut map = Map::new();
        loop {
            match self.peek() {
                Some(Token::CloseBrace) => {
                    self.take();
                    break;
                }
                None => return Err("unexpected end of hashtable".to_string()),
                _ => {}
            }
            let key = match self.take() {
                Some(Token::Word(value)) => value,
                other => return Err(format!("expected hashtable key, got {other:?}")),
            };
            match self.take() {
                Some(Token::Equals) => {}
                other => return Err(format!("expected = after key, got {other:?}")),
            };
            map.insert(key, self.value()?);
            if matches!(self.peek(), Some(Token::Separator)) {
                self.take();
            }
        }
        Ok(Value::Object(map))
    }
    fn array(&mut self) -> Result<Value, String> {
        let mut values = Vec::new();
        loop {
            match self.peek() {
                Some(Token::CloseParen) => {
                    self.take();
                    break;
                }
                None => return Err("unexpected end of array".to_string()),
                _ => {}
            }
            values.push(self.value()?);
            if matches!(self.peek(), Some(Token::Separator)) {
                self.take();
            }
        }
        Ok(Value::Array(values))
    }
}

fn tokenize(source: &str) -> Vec<Token> {
    let chars: Vec<char> = source.chars().collect();
    let mut out = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() {
            index += 1;
            continue;
        }
        if ch == '#' {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            continue;
        }
        let token = match ch {
            '@' => Some(Token::At),
            '{' => Some(Token::OpenBrace),
            '}' => Some(Token::CloseBrace),
            '(' => Some(Token::OpenParen),
            ')' => Some(Token::CloseParen),
            '=' => Some(Token::Equals),
            ';' | ',' => Some(Token::Separator),
            _ => None,
        };
        if let Some(token) = token {
            out.push(token);
            index += 1;
            continue;
        }
        if ch == '\'' || ch == '"' {
            let quote = ch;
            index += 1;
            let mut value = String::new();
            while index < chars.len() {
                let current = chars[index];
                index += 1;
                if current == quote {
                    if quote == '\'' && index < chars.len() && chars[index] == '\'' {
                        value.push('\'');
                        index += 1;
                        continue;
                    }
                    break;
                }
                value.push(current);
            }
            out.push(Token::Word(value));
            continue;
        }
        let mut value = String::new();
        while index < chars.len() {
            let current = chars[index];
            if current.is_whitespace() || "@{}()=;,".contains(current) || current == '#' {
                break;
            }
            value.push(current);
            index += 1;
        }
        if !value.is_empty() {
            out.push(Token::Word(value));
        } else {
            index += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_nested_psd1_agents() {
        let source = "@{ NaradaRoot = 'C:/Narada'; Agents = @(@{ Agent = 'site.user'; Role = 'user'; EnableNativeShell = $true }); }";
        let value = Parser::new(source).parse().expect("parse");
        assert_eq!(value["Agents"][0]["Agent"], "site.user");
        assert_eq!(value["Agents"][0]["EnableNativeShell"], true);
    }
    #[test]
    fn scope_loci_are_bounded() {
        assert_eq!(scope_loci("all"), vec!["host", "user-site", "local-site"]);
        assert!(scope_loci("none").is_empty());
    }

    #[test]
    fn plan_uses_direct_operator_surface_runtime_contract() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let test_root = std::env::temp_dir().join(format!(
            "narada-launcher-native-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&test_root).expect("create test root");
        let registry = test_root.join("agents.psd1");
        std::fs::write(
            &registry,
            format!(
                "@{{ NaradaRoot = '{}'; WorkspaceRoot = '{}'; SiteRoot = '{}'; Launcher = 'site.ps1'; OperatorSurface = 'agent-cli'; Runtime = 'narada-agent-runtime-server'; Agents = @(@{{ Agent = 'site.architect'; Role = 'architect'; McpScope = 'all'; }}); }}",
                path_text(&test_root),
                path_text(&test_root),
                path_text(&test_root)
            ),
        )
        .expect("write registry");

        let mut args = Map::new();
        args.insert("agent".to_string(), json!(["site.architect"]));
        let result = plan(&args, &test_root, Some(&registry)).expect("plan");
        let argv: Vec<&str> = result["wt_args"]
            .as_array()
            .expect("wt args")
            .iter()
            .filter_map(Value::as_str)
            .collect();

        assert!(argv.iter().any(|value| *value == "--resident-runtime-host"));
        assert!(argv.iter().any(
            |value| value.ends_with("narada-agent-runtime-server-rust.exe")
                || value.ends_with("narada-agent-runtime-server-rust")
        ));
        assert!(!argv
            .iter()
            .any(|value| matches!(*value, "pnpm" | "node" | "bun")));
        assert_eq!(
            result["command_contract"],
            "narada.native_launch_compilation.v1"
        );
        assert_eq!(result["native_launches"][0]["status"], "compiled");
        assert!(argv
            .windows(2)
            .any(|window| window == ["--identity", "site.architect"]));
        assert!(argv
            .windows(2)
            .any(|window| window == ["--authority", "auto"]));
        assert!(argv
            .windows(2)
            .any(|window| window == ["--mcp-scope", "all"]));
        assert!(!argv.iter().any(|arg| arg.contains("Start-NaradaAgent.ps1")));
        assert!(!argv.contains(&"-LauncherPath"));
        assert!(!argv.contains(&"--wait"));

        std::fs::remove_dir_all(test_root).expect("remove test root");
    }
}
