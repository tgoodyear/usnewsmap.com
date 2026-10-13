//! Named-flag argument parsing. No positional arguments, except where BOINC itself calls a
//! program with them (see the validator and assimilator binaries).

use std::collections::BTreeMap;

#[derive(Debug, Default, PartialEq)]
pub struct Flags {
    values: BTreeMap<String, String>,
    switches: Vec<String>,
}

impl Flags {
    /// Parse `--name value` pairs and `--switch` flags. `switch_names` lists the flags that
    /// take no value. Anything not starting with `--` is an error.
    pub fn parse(args: &[String], switch_names: &[&str]) -> Result<Flags, String> {
        let mut flags = Flags::default();
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            let Some(name) = a.strip_prefix("--") else {
                return Err(format!("unexpected argument {a:?}; use named --flags"));
            };
            if switch_names.contains(&name) {
                flags.switches.push(name.to_string());
                i += 1;
            } else {
                let Some(v) = args.get(i + 1) else {
                    return Err(format!("--{name} needs a value"));
                };
                flags.values.insert(name.to_string(), v.clone());
                i += 2;
            }
        }
        Ok(flags)
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn require(&self, name: &str) -> Result<&str, String> {
        self.get(name)
            .ok_or_else(|| format!("--{name} is required"))
    }

    pub fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }

    pub fn get_parsed<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T, String> {
        match self.get(name) {
            None => Ok(default),
            Some(v) => v
                .parse()
                .map_err(|_| format!("--{name}: cannot parse {v:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_values_and_switches() {
        let f = Flags::parse(&s(&["--a", "1", "--dry-run", "--b", "x"]), &["dry-run"]).unwrap();
        assert_eq!(f.get("a"), Some("1"));
        assert!(f.has("dry-run"));
        assert_eq!(f.require("b").unwrap(), "x");
        assert_eq!(f.get_parsed("a", 0u32).unwrap(), 1);
        assert_eq!(f.get_parsed("missing", 7u32).unwrap(), 7);
    }

    #[test]
    fn rejects_positional_and_missing_value() {
        assert!(Flags::parse(&s(&["pos"]), &[]).is_err());
        assert!(Flags::parse(&s(&["--a"]), &[]).is_err());
        assert!(Flags::parse(&s(&[]), &[]).unwrap().require("x").is_err());
    }
}
