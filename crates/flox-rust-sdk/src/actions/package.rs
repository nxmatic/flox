use derive_more::Constructor;

use runix::{
    arguments::{
        flake::{FlakeArgs, OverrideInputs},
        NixArgs,
    },
    command::Build,
    installable::Installable,
    NixBackend, Run,
};
use thiserror::Error;

use crate::{
    flox::{Flox, FloxNixApi},
    prelude::Stability,
};

#[derive(Constructor)]
pub struct Package<'flox> {
    flox: &'flox Flox,
    installable: Installable,
    stability: Stability,
}

#[derive(Error, Debug)]
pub enum PackageBuildError<Nix: NixBackend>
where
    Build: Run<Nix>,
{
    #[error("Error getting Nix instance")]
    NixInstance(()),
    #[error("Error getting flake args")]
    FlakeArgs(()),
    #[error("Error running nix: {0}")]
    NixRun(<Build as Run<Nix>>::Error),
}

impl Package<'_> {
    fn flake_args(&self) -> Result<FlakeArgs, ()> {
        Ok(FlakeArgs {
            override_inputs: Some(vec![OverrideInputs::new(
                "floxpkgs/nixpkgs/nixpkgs".into(),
                format!("flake:nixpkgs-{}", self.stability),
            )
            .into()]),
        })
    }

    /// flox build
    /// runs `nix build <installable>`
    pub async fn build<Nix: FloxNixApi>(&self) -> Result<(), PackageBuildError<Nix>>
    where
        Build: Run<Nix>,
    {
        let nix = self.flox.nix::<Nix>();

        let nix_args = NixArgs::default();

        let command = Build {
            flake: self.flake_args().map_err(PackageBuildError::FlakeArgs)?,
            installables: [self.installable.clone()].into(),
            ..Default::default()
        };

        command
            .run(&nix, &nix_args)
            .await
            .map_err(PackageBuildError::NixRun)?;

        Ok(())
    }
}
