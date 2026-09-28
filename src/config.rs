use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// NSE market route used when requesting a symbol's corporate announcements.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MarketApiType {
    /// Main-board listings. NSE uses the API value `equities`.
    #[serde(rename = "equities")]
    Equities,
    /// SME listings. NSE uses the API value `sme`.
    Sme,
}

impl MarketApiType {
    /// Return the value expected by the NSE announcement endpoint.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Equities => "equities",
            Self::Sme => "sme",
        }
    }
}

/// One NSE symbol and the market route needed to query it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StockEntry {
    /// NSE trading symbol, stored in uppercase.
    pub symbol: String,
    /// Main-board (`equities`) or SME (`sme`) API route.
    pub market_api_type: MarketApiType,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StockList {
    pub entries: Vec<StockEntry>,
}

impl Default for StockList {
    fn default() -> Self {
        Self {
            entries: vec![StockEntry {
                symbol: "ALPEXSOLAR".to_owned(),
                market_api_type: MarketApiType::Sme,
            }],
        }
    }
}

impl StockList {
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("could not read stock list {}", path.display()))?;
        let entries: Vec<StockEntry> = serde_json::from_str(&content)
            .with_context(|| format!("invalid JSON in {}", path.display()))?;
        let mut list = Self { entries };
        for stock in &mut list.entries {
            stock.symbol = normalize_symbol(&stock.symbol)?;
        }
        Ok(list)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
        let serialized = serde_json::to_string_pretty(&self.entries)?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, format!("{serialized}\n"))
            .with_context(|| format!("could not write {}", temporary.display()))?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("could not replace {}", path.display()))
    }
}

pub(crate) fn normalize_symbol(raw: &str) -> Result<String> {
    let symbol = raw.trim().to_ascii_uppercase();
    if symbol.is_empty()
        || !symbol
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'&' | b'-' | b'.'))
    {
        bail!("invalid NSE symbol `{raw}`; use letters, digits, &, - or .")
    }
    Ok(symbol)
}
