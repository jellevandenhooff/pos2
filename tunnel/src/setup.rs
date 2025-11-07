use anyhow::{Result, bail};
use std::fmt::Display;

use crate::client::ClientState;
use crate::common::Environment;

#[derive(Clone)]
enum TunnelChoice {
    Known(String),
    Custom,
}

impl Display for TunnelChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Known(domain) => write!(f, "use tunnel server {}", domain),
            Self::Custom => write!(f, "use custom tunnel server"),
        }
    }
}

#[derive(Clone)]
enum DomainChoice {
    Existing(String),
    WithSuffix(String),
}

impl Display for DomainChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Existing(domain) => write!(f, "use existing domain {}", domain),
            Self::WithSuffix(suffix) => write!(f, "new domain ending in {}", suffix),
        }
    }
}

pub async fn run_interactive_setup(
    env: &Environment,
    endpoints: Vec<String>,
) -> Result<ClientState> {
    let mut choices = Vec::new();
    choices.extend(
        endpoints
            .iter()
            .map(|endpoint| TunnelChoice::Known(endpoint.clone())),
    );
    choices.push(TunnelChoice::Custom);

    let endpoint = {
        let choice =
            inquire::Select::new("what tunnel server would you like to use?", choices.clone())
                .prompt()?;

        println!("selected choice, matching...");
        let endpoint = match choice {
            TunnelChoice::Known(endpoint) => {
                println!("known endpoint: {}", endpoint);
                endpoint
            }
            TunnelChoice::Custom => {
                println!("custom endpoint");
                let endpoint = inquire::prompt_text("please enter a endpoint")?;
                endpoint
            }
        };
        println!("endpoint resolved to: {}", endpoint);
        endpoint
    };

    println!("calling run_device with endpoint: {}", endpoint);
    let token = crate::web::run_device(&env, &endpoint).await?;
    println!("run_device returned");

    let client = crate::web::ApiClient::new(&env, &&endpoint, &token);

    let info = client.test_request(&crate::web::ApiTestRequest {}).await?;

    let mut choices = Vec::new();
    choices.extend(
        info.domains
            .iter()
            .map(|domain| DomainChoice::Existing(domain.clone())),
    );
    choices.extend(
        info.available_suffixes
            .iter()
            .map(|suffix| DomainChoice::WithSuffix(suffix.clone())),
    );

    if choices.len() == 0 {
        bail!("sorry, you have no domains");
    }

    let (domain, new) = loop {
        let choice =
            inquire::Select::new("what domain would you like to use?", choices.clone())
                .prompt()?;

        let (domain, new) = match choice {
            DomainChoice::Existing(domain) => (domain, false),
            DomainChoice::WithSuffix(suffix) => {
                let prefix = inquire::prompt_text("please pick a name before the suffix")?;
                let domain = format!("{}{}", prefix, suffix);
                (domain, true)
            }
        };

        println!("got: {}", domain);

        let ok = inquire::prompt_confirmation(format!(
            "continue with domain {}? this will go and get a domain [yes/no]",
            &domain
        ))?;
        if ok {
            break (domain, new);
        }
    };

    if new {
        let _resp = client
            .register_domain(&crate::web::ApiRegisterDomainRequest {
                domain: domain.clone(),
            })
            .await?;
        println!("registered: {}", domain);
    }

    println!("domain: {} token: {}", domain, token);

    Ok(ClientState {
        endpoint: endpoint.clone(),
        token: token.clone(),
        domain: domain.clone(),
    })
}
