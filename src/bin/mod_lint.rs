use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Cursor, Read, Seek},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use colored::Colorize;
use repak::PakReader;
use unreal_asset::{reader::asset_trait::AssetTrait, Asset};

fn main() -> Result<()> {
    // https://github.com/mackwic/colored/issues/110
    #[cfg(windows)]
    {
        let _varname = colored::control::set_virtual_terminal(true).unwrap_or(());
    }

    if let Some(url) = std::env::args().nth(1) {
        let mut reader = get_pak(&url)?;
        let pak = PakReader::new_any(&mut reader, None)?;
        let mount_point = PathBuf::from(pak.mount_point());
        if let Ok(sanitized) = mount_point.strip_prefix("../../../") {
            let valid_extensions =
                vec!["uasset", "uexp", "umap", "ubulk", "ufont", "ini", "locres"]
                    .into_iter()
                    .collect::<HashSet<_>>();
            let mut extraneous_files = HashSet::new();
            let mut extensions = HashMap::new();
            for f in pak.files() {
                let path = Path::new(&f);
                let no_ext = String::from(path.with_extension("").to_string_lossy());
                if let Some(ext) = path.extension() {
                    let ext = String::from(ext.to_string_lossy());
                    if !valid_extensions.contains(ext.as_str()) {
                        extraneous_files.insert(f);
                    }
                    extensions
                        .entry(no_ext)
                        .or_insert(HashSet::new())
                        .insert(ext);
                } else {
                    extraneous_files.insert(f);
                }
            }
            let extraneous_files = extraneous_files
                .into_iter()
                .map(|f| String::from(sanitized.join(f).to_string_lossy()))
                .filter(|f| f != "FSD/AssetRegistry.bin")
                .collect::<BTreeSet<_>>();
            if !extraneous_files.is_empty() {
                println!("{}", "extraneous files:".bold());
                for f in extraneous_files {
                    println!("\t{f}");
                }
            }
            let mut split_pairs = BTreeSet::new();
            let mut asset_types = BTreeMap::new();
            for (f, ext) in extensions {
                let uasset = ext.contains("uasset");
                let umap = ext.contains("umap");
                let uexp = ext.contains("uexp");
                if (umap || uasset) != uexp {
                    for e in ext {
                        split_pairs.insert(String::from(
                            sanitized.join(&f).with_extension(e).to_string_lossy(),
                        ));
                    }
                } else if (umap || uasset) && uexp {
                    let uasset = Cursor::new(pak.get(
                        &if uasset {
                            format!("{f}.uasset")
                        } else {
                            format!("{f}.umap")
                        },
                        &mut reader,
                    )?);
                    let uexp = Cursor::new(pak.get(&format!("{f}.uexp"), &mut reader)?);
                    std::panic::set_hook(Box::new(|_info| {}));
                    let result = std::panic::catch_unwind(|| {
                        let mut asset = unreal_asset::Asset::new(uasset, Some(uexp));
                        asset.set_engine_version(
                            unreal_asset::engine_version::EngineVersion::VER_UE4_27,
                        );
                        asset
                            .parse_data()
                            .map_err(|_| anyhow!("failed to parse asset"))
                            .and_then(|_| get_type(&asset))
                    })
                    .map_err(|_| anyhow!("failed to parse asset"));
                    asset_types.insert(
                        String::from(sanitized.join(f).to_string_lossy()),
                        //result.map_or_else(|e| e, |e| Box::new(e)),
                        result.and_then(|e| e),
                    );
                }
            }
            if !split_pairs.is_empty() {
                println!("{}", "split asset pairs:".bold());
                for f in split_pairs {
                    println!("\t{f}");
                }
            }
            if !asset_types.is_empty() {
                let auto_verified = [
                    "SoundWave",
                    "SoundCue",
                    "SoundClass",
                    "SoundMix",
                    "MaterialInstanceConstant",
                    "Material",
                    "SkeletalMesh",
                    "StaticMesh",
                    "Texture2D",
                    "AnimSequence",
                    "Skeleton",
                    "StringTable",
                ]
                .into_iter()
                .collect::<HashSet<_>>();

                let mut auto_verified_results = asset_types
                    .into_iter()
                    .map(|(f, t)| {
                        let auto_verify = match &t {
                            Ok(t) => {
                                if auto_verified.contains(t.as_str()) {
                                    AutoVerify::Pass
                                } else {
                                    AutoVerify::Fail
                                }
                            }
                            _ => AutoVerify::Unknown,
                        };
                        let msg = match t {
                            Ok(t) => AssetType::Known(t),
                            Err(e) => AssetType::Unknown(format!("{e}")),
                        };
                        (auto_verify, msg, f)
                    })
                    .collect::<Vec<_>>();

                auto_verified_results.sort();

                println!(
                    "{:12} {:30} {}",
                    "auto-verify".bold(),
                    "class".bold(),
                    "asset path".bold()
                );
                for (a, m, f) in auto_verified_results {
                    println!("{:^12} {:30} {}", a.output(), m.output(), f);
                }
            }
        } else {
            return Err(anyhow!(
                "Invalid mount point: {}, should begin with \"../../../\"",
                pak.mount_point()
            ));
        }
    } else {
        println!("Usage: {} <mod .pak or .zip>", env!("CARGO_BIN_NAME"))
    }
    Ok(())
}

#[derive(Debug, Ord, Eq, PartialEq, PartialOrd)]
enum AutoVerify {
    Pass,
    Fail,
    Unknown,
}

impl AutoVerify {
    fn output(&self) -> colored::ColoredString {
        match self {
            AutoVerify::Pass => "yes".green(),
            AutoVerify::Fail => "no".red(),
            AutoVerify::Unknown => "?".yellow(),
        }
    }
}

#[derive(Debug, Ord, Eq, PartialEq, PartialOrd)]
enum AssetType {
    Known(String),
    Unknown(String),
}

impl AssetType {
    fn output(&self) -> colored::ColoredString {
        match self {
            AssetType::Known(s) => s.normal(),
            AssetType::Unknown(s) => s.yellow(),
        }
    }
}

fn get_type<R: Read + Seek>(asset: &Asset<R>) -> Result<String> {
    use unreal_asset::exports::ExportBaseTrait;

    for e in &asset.exports {
        let base = e.get_base_export();
        if base.outer_index.index == 0 {
            return Ok(asset
                .get_import(base.class_index)
                .ok_or_else(|| anyhow!("missing class import"))?
                .object_name
                .content
                .to_owned());
        }
    }
    Err(anyhow!("could not determine asset class"))
}

trait Reader: BufRead + Seek {}
impl<T> Reader for T where T: BufRead + Seek {}

fn get_pak(url: &str) -> Result<Box<dyn Reader>> {
    let re = regex::Regex::new(
        r"^https?://(mod\.io/g/drg/m/|drg\.(old\.)?mod\.io/)(?P<name_id>[^/#]+)$",
    )
    .unwrap();

    let reader: Box<dyn Reader> = if let Some(captures) = re.captures(url) {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .enable_io()
            .build()
            .unwrap()
            .block_on(async { get_modio_mod(captures.name("name_id").unwrap().as_str()).await })?
    } else {
        Box::new(BufReader::new(File::open(url)?))
    };

    get_pak_from_data(reader)
}

fn get_pak_from_data(mut data: Box<dyn Reader>) -> Result<Box<dyn Reader>> {
    if let Ok(mut archive) = zip::ZipArchive::new(&mut data) {
        (0..archive.len())
            .map(|i| -> Result<Option<Box<dyn Reader>>> {
                let mut file = archive.by_index(i)?;
                match file.enclosed_name() {
                    Some(p) => {
                        if file.is_file() && p.extension().filter(|e| e == &"pak").is_some() {
                            let mut buf = vec![];
                            file.read_to_end(&mut buf)?;
                            Ok(Some(Box::new(Cursor::new(buf))))
                        } else {
                            Ok(None)
                        }
                    }
                    None => Ok(None),
                }
            })
            .find_map(|e| e.transpose())
            .ok_or_else(|| anyhow!("Zip does not contain pak"))?
    } else {
        data.rewind()?;
        Ok(data)
    }
}

fn get_modio_key() -> Result<String> {
    use directories::BaseDirs;
    let key_path = if let Some(base_dirs) = BaseDirs::new() {
        let dir = base_dirs.config_dir().join(env!("CARGO_PKG_NAME"));
        Some((dir.join("modio_key.txt"), dir))
    } else {
        eprintln!("could not determine config path to save key");
        None
    };

    let key = key_path.as_ref().and_then(|p| {
        std::fs::read_to_string(&p.0)
            .ok()
            .map(|k| k.trim().to_owned())
    });
    Ok(if let Some(key) = key {
        key
    } else {
        println!("No saved modio API key found, please generate one by going to https://mod.io/me/access#api and pasting it here");
        let key = rpassword::prompt_password("API key: ")?;
        if let Some(key_path) = key_path {
            std::fs::create_dir_all(&key_path.1)?;
            println!("writing modio API key to {}", key_path.0.display());
            std::fs::write(key_path.0, &key)?;
        }
        key
    })
}

const MODIO_DRG_ID: u32 = 2475;
async fn get_modio_mod(name_id: &str) -> Result<Box<dyn Reader>> {
    let modio = modio::Modio::new(modio::Credentials::new(get_modio_key()?))?;

    use modio::filter::Eq;

    let mut mods = modio
        .game(MODIO_DRG_ID)
        .mods()
        .search(modio::mods::filters::NameId::eq(name_id))
        .collect()
        .await?;
    if mods.len() > 1 {
        Err(anyhow!(
            "multiple mods returned for mod name_id {}",
            name_id,
        ))
    } else if let Some(mod_) = mods.pop() {
        let file = mod_
            .modfile
            .ok_or_else(|| anyhow!("mod {name_id} does not have an associated modfile"))?;

        let filename = file.filename.to_owned();
        println!(
            "downloading mod {} file_id={} to {}...",
            name_id, file.id, filename
        );

        use futures_util::TryStreamExt;
        use tokio::io::AsyncWriteExt;

        let download_bar = indicatif::ProgressBar::new(file.filesize);
        download_bar.set_style(indicatif::ProgressStyle::with_template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")?.progress_chars("#>-"));

        let mut stream = Box::pin(
            modio
                .download(modio::download::DownloadAction::FileObj(Box::new(file)))
                .stream(),
        );
        let mut cursor = Cursor::new(vec![]);
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(filename)
            .await?;
        while let Some(bytes) = stream.try_next().await? {
            cursor.write_all(&bytes).await?;
            file.write_all(&bytes).await?;
            download_bar.inc(bytes.len() as u64);
        }

        Ok(Box::new(cursor))
    } else {
        Err(anyhow!("no mods returned for mod name_id {}", &name_id))
    }
}
