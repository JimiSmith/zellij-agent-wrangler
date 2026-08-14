const GIVING_UP: u32 = 5;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Client {
    #[default]
    Unnamed,
    Untried(String),
    Working(String),
    Failing {
        path: String,
        failures: u32,
    },
    Broken {
        why: String,
    },
}

impl Client {
    pub fn new(path: &str) -> Self {
        Client::Untried(path.to_string())
    }

    pub fn path(&self) -> Option<&str> {
        match self {
            Client::Untried(path) | Client::Working(path) | Client::Failing { path, .. } => {
                Some(path)
            }
            Client::Unnamed | Client::Broken { .. } => None,
        }
    }

    pub fn reached(&mut self) -> bool {
        match self {
            Client::Untried(path) | Client::Failing { path, .. } => {
                *self = Client::Working(path.clone());
                true
            }
            _ => false,
        }
    }

    pub fn failed(&mut self, call: &str, said: &str) -> bool {
        let why = if said.is_empty() {
            call.to_string()
        } else {
            format!("{call}: {said}")
        };
        let next = match self {
            Client::Unnamed => return false,
            Client::Untried(_) => Client::Broken { why },
            Client::Working(path) => Client::Failing {
                path: path.clone(),
                failures: 1,
            },
            Client::Failing { path, failures } if *failures + 1 < GIVING_UP => Client::Failing {
                path: path.clone(),
                failures: *failures + 1,
            },
            Client::Failing { .. } | Client::Broken { .. } => Client::Broken { why },
        };
        if *self == next {
            false
        } else {
            *self = next;
            true
        }
    }

    pub fn why(&self) -> Option<&str> {
        match self {
            Client::Broken { why } => Some(why),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untried_client_is_given_up_on_at_the_first_failure() {
        let mut client = Client::new("missing");
        assert!(client.failed("register", "not found"));
        assert_eq!(client.path(), None);
        assert_eq!(client.why(), Some("register: not found"));
    }

    #[test]
    fn a_working_client_survives_a_transient_failure() {
        let mut client = Client::new("agent-wrangler");
        client.reached();
        client.failed("seen", "busy");
        assert_eq!(client.path(), Some("agent-wrangler"));
        assert_eq!(client.why(), None);
        assert!(client.reached());
    }

    #[test]
    fn repeated_failures_eventually_retire_a_working_client() {
        let mut client = Client::new("agent-wrangler");
        client.reached();
        for _ in 0..GIVING_UP {
            client.failed("seen", "gone");
        }
        assert_eq!(client.path(), None);
        assert_eq!(client.why(), Some("seen: gone"));
    }

    #[test]
    fn a_success_resets_the_run_of_failures() {
        let mut client = Client::new("agent-wrangler");
        client.reached();
        for _ in 0..GIVING_UP * 3 {
            client.failed("seen", "busy");
            assert_eq!(client.why(), None);
            assert!(client.reached());
        }
        assert_eq!(client.path(), Some("agent-wrangler"));
    }
}
