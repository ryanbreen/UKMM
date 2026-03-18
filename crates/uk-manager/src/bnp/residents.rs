use std::path::Path;

use anyhow::Result;
use fs_err as fs;
use roead::byml::Byml;
use uk_content::{
    actor::residents::ResidentActorData, prelude::Resource, resource::ResidentActors,
};

use super::BnpConverter;

impl BnpConverter {
    pub fn handle_residents(&self) -> Result<()> {
        let residents_path = self.current_root.join("logs/residents.yml");
        if residents_path.exists() {
            log::debug!("Processing resident actors log");
            let diff = Byml::from_text(fs::read_to_string(residents_path)?)?.into_map()?;
            let data = self.get_from_master_sarc("Pack/Bootup.pack//Actor/ResidentActors.byml")?;
            if let Ok(mut residents) = ResidentActors::from_binary(data) {
                let mut added = Vec::new();
                let mut skipped = Vec::new();

                for (name, data) in diff {
                    if let Some(actor_data) = data
                        .as_map()
                        .ok()
                        .and_then(|m| ResidentActorData::try_from(m).ok())
                    {
                        // Check that the actor pack actually exists — either in the game
                        // dump or in the mod's merged output. A missing pack will cause a
                        // null pointer crash at runtime when BotW tries to pre-load it.
                        let pack_name = format!("{}.sbactorpack", &name);
                        let pack_path =
                            Path::new("Actor/Pack").join(&pack_name);
                        let exists_in_dump = self
                            .dump
                            .get_bytes_uncached(&pack_path)
                            .is_ok();
                        let exists_in_mod = self
                            .packs
                            .iter()
                            .any(|p| p.ends_with(&pack_name));

                        if exists_in_dump || exists_in_mod {
                            residents.0.insert(name.clone(), actor_data);
                            added.push(name);
                        } else {
                            log::error!(
                                "SKIPPING resident actor '{}': no actor pack found in game \
                                 dump or mod files. Including it would crash the game.",
                                &name
                            );
                            skipped.push(name);
                        }
                    }
                }

                if !added.is_empty() {
                    log::info!(
                        "Added {} resident actor(s): {}",
                        added.len(),
                        added.join(", ")
                    );
                }
                if !skipped.is_empty() {
                    log::warn!(
                        "Skipped {} resident actor(s) with missing packs: {}",
                        skipped.len(),
                        skipped.join(", ")
                    );
                }

                self.inject_into_sarc(
                    "Pack/Bootup.pack//Actor/ResidentActors.byml",
                    residents.into_binary(self.platform.into()),
                    false,
                )?;
            }
        }
        Ok(())
    }
}
