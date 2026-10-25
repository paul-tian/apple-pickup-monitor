//! Region-scoped product snapshots, loaded only when that region is requested.
use std::{collections::HashMap, io::Read, path::PathBuf};

use apw_core::{
    catalog::Catalog,
    model::{Category, Product, region_by_locale},
};
use tokio::sync::RwLock;

const MAX_CACHE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone)]
pub struct CatalogSnapshot {
    pub products: Vec<Product>,
    pub warning: Option<String>,
}

pub struct ProductCatalogs {
    directory: PathBuf,
    regions: RwLock<HashMap<String, CatalogSnapshot>>,
}

impl ProductCatalogs {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            regions: RwLock::new(HashMap::new()),
        }
    }

    pub async fn get(&self, locale: &str, upstream: &Catalog) -> Result<CatalogSnapshot, String> {
        if region_by_locale(locale).is_none() {
            return Err("This Apple store region is not supported.".into());
        }
        if let Some(snapshot) = self.regions.read().await.get(locale).cloned() {
            return Ok(snapshot);
        }
        // A write lock also avoids duplicate disk loads for concurrent first requests.
        let mut regions = self.regions.write().await;
        if let Some(snapshot) = regions.get(locale) {
            return Ok(snapshot.clone());
        }
        let mut snapshot = CatalogSnapshot {
            products: upstream.products(locale).map_err(|e| e.to_string())?,
            warning: None,
        };
        let path = self.path(locale);
        match std::fs::File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                let cached = file
                    .take(MAX_CACHE_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .ok()
                    .filter(|_| bytes.len() as u64 <= MAX_CACHE_BYTES)
                    .and_then(|_| serde_json::from_slice::<Vec<Product>>(&bytes).ok())
                    .filter(|products| !products.is_empty());
                match cached {
                    Some(products) => snapshot.products = products,
                    None => {
                        snapshot.warning = Some(format!(
                            "The cached product catalogue for {locale} could not be read. Using the bundled catalogue."
                        ))
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                snapshot.warning = Some(format!(
                    "The cached product catalogue for {locale} could not be read. Using the bundled catalogue."
                ))
            }
        }
        regions.insert(locale.into(), snapshot.clone());
        Ok(snapshot)
    }

    pub async fn replace_category(
        &self,
        locale: &str,
        category: Category,
        fresh: Vec<Product>,
        upstream: &Catalog,
    ) -> Result<(), String> {
        let current = self.get(locale, upstream).await?;
        let mut merged: Vec<Product> = current
            .products
            .into_iter()
            .filter(|p| p.category != category)
            .collect();
        let replacement: Vec<Product> = fresh
            .into_iter()
            .filter(|p| p.category == category)
            .collect();
        if replacement.is_empty() {
            return Err(
                "Apple returned no products for this category. The saved catalogue is unchanged."
                    .into(),
            );
        }
        merged.extend(replacement);
        let bytes = serde_json::to_vec(&merged).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
        let path = self.path(locale);
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&temp, &path).map_err(|e| e.to_string())?;
        self.regions.write().await.insert(
            locale.into(),
            CatalogSnapshot {
                products: merged,
                warning: None,
            },
        );
        Ok(())
    }

    fn path(&self, locale: &str) -> PathBuf {
        self.directory.join(format!("products-{locale}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn regional_caches_remain_isolated_and_preserve_legacy_au_file() {
        let dir = tempfile::tempdir().unwrap();
        let upstream = Catalog::new();
        let mut australian = upstream.products("en_AU").unwrap();
        australian[0].title = "Saved Australian catalogue".into();
        std::fs::write(
            dir.path().join("products-en_AU.json"),
            serde_json::to_vec(&australian).unwrap(),
        )
        .unwrap();
        let caches = ProductCatalogs::new(dir.path().into());
        assert_eq!(
            caches.get("en_AU", &upstream).await.unwrap().products[0].title,
            "Saved Australian catalogue"
        );
        let us = caches.get("en_US", &upstream).await.unwrap().products;
        assert_ne!(us[0].title, "Saved Australian catalogue");
        let mut refreshed = us.clone();
        let category = refreshed[0].category;
        refreshed[0].title = "Updated US product".into();
        caches
            .replace_category("en_US", category, refreshed, &upstream)
            .await
            .unwrap();
        assert!(dir.path().join("products-en_US.json").exists());
        let reloaded = ProductCatalogs::new(dir.path().into());
        assert_eq!(
            reloaded.get("en_AU", &upstream).await.unwrap().products[0].title,
            "Saved Australian catalogue"
        );
        assert!(
            reloaded
                .get("en_US", &upstream)
                .await
                .unwrap()
                .products
                .iter()
                .any(|p| p.title == "Updated US product")
        );
        assert!(reloaded.get("../../outside", &upstream).await.is_err());
    }
}
