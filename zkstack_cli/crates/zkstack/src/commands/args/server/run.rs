use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};

use crate::{
    messages::{
        MSG_SERVER_ADDITIONAL_ARGS_HELP, MSG_SERVER_COMPONENTS_HELP, MSG_SERVER_GENESIS_HELP,
        MSG_SERVER_URING_HELP,
    },
    utils::docker::select_tag,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize, ValueEnum)]
pub enum ExecutionMode {
    #[default]
    Release,
    Debug,
    Docker,
}

impl From<ExecutionMode> for common::server::ExecutionMode {
    fn from(mode: ExecutionMode) -> Self {
        match mode {
            ExecutionMode::Debug => Self::Debug,
            ExecutionMode::Release => Self::Release,
            ExecutionMode::Docker => Self::Docker,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Parser)]
pub struct RunServerArgs {
    #[arg(long, default_value = "release")]
    pub mode: ExecutionMode,
    #[arg(long)]
    pub tag: Option<String>,
    #[arg(long, help = MSG_SERVER_COMPONENTS_HELP)]
    pub components: Option<Vec<String>>,
    #[arg(long, help = MSG_SERVER_GENESIS_HELP)]
    pub genesis: bool,
    #[arg(
        long, short,
        trailing_var_arg = true,
        allow_hyphen_values = true,
        hide = false,
        help = MSG_SERVER_ADDITIONAL_ARGS_HELP
    )]
    additional_args: Vec<String>,
    #[clap(help = MSG_SERVER_URING_HELP, long, default_missing_value = "true")]
    pub uring: bool,
}

impl RunServerArgs {
    pub async fn fill_values_with_prompt(self) -> RunServerArgsFinal {
        let tag = if let ExecutionMode::Docker = self.mode {
            self.tag
                .or(select_tag().await.ok().or(Some("latest".to_string())))
        } else {
            None
        };

        RunServerArgsFinal {
            mode: self.mode,
            tag,
            components: self.components,
            genesis: self.genesis,
            additional_args: self.additional_args,
            uring: self.uring,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Parser)]
pub struct RunServerArgsFinal {
    pub mode: ExecutionMode,
    pub tag: Option<String>,
    pub components: Option<Vec<String>>,
    pub genesis: bool,
    pub additional_args: Vec<String>,
    pub uring: bool,
}