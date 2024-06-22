use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, Cursor, Read, Seek},
};

use anyhow::{anyhow, bail, Context, Result};
use colored::Colorize;
use repak::PakBuilder;
use unreal_asset::{exports::ExportBaseTrait, reader::ArchiveTrait, types::PackageIndex, Asset};

use typed_path::Utf8UnixComponent as PakPathComponent;
use typed_path::Utf8UnixPath as PakPath;

fn main() -> Result<()> {
    // https://github.com/mackwic/colored/issues/110
    #[cfg(windows)]
    {
        let _varname = colored::control::set_virtual_terminal(true).unwrap_or(());
    }

    if let Some(url) = std::env::args().nth(1) {
        let mut reader = get_pak(&url)?;
        let pak = PakBuilder::new().reader(&mut reader)?;
        let mount_point = PakPath::new(pak.mount_point());
        if let Ok(sanitized) = mount_point.strip_prefix("../../../") {
            let valid_extensions =
                vec!["uasset", "uexp", "umap", "ubulk", "ufont", "ini", "locres"]
                    .into_iter()
                    .collect::<BTreeSet<_>>();
            let mut extraneous_files: BTreeSet<String> = Default::default();
            let mut extensions: BTreeMap<String, BTreeSet<String>> = Default::default();
            for f in pak.files() {
                let path = PakPath::new(&f);
                if let Some(ext) = path.extension() {
                    if !valid_extensions.contains(ext) {
                        extraneous_files.insert(f.to_owned());
                    }
                    extensions
                        .entry(path.with_extension("").to_string())
                        .or_default()
                        .insert(ext.to_owned());
                } else {
                    extraneous_files.insert(f.to_owned());
                }
            }
            let extraneous_files = extraneous_files
                .into_iter()
                .map(|f| sanitized.join(f))
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
            let mut hierarchy: BTreeMap<String, BTreeSet<String>> = Default::default();
            for (f, ext) in extensions {
                let uasset = ext.contains("uasset");
                let umap = ext.contains("umap");
                let uexp = ext.contains("uexp");
                if (umap || uasset) != uexp {
                    for e in ext {
                        split_pairs.insert(sanitized.join(&f).with_extension(e));
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

                    let pak_path = sanitized.join(&f);
                    let path = pak_path_to_game_path(pak_path)?;

                    let asset = unreal_asset::Asset::new(
                        uasset,
                        None,
                        unreal_asset::engine_version::EngineVersion::VER_UE4_27,
                        None,
                        true,
                    )
                    .context("failed to parse asset")?;

                    if let Some(parent_path) = get_parent_path(&asset)? {
                        let full_path = get_full_path(&path, &asset)?;
                        hierarchy.entry(parent_path).or_default().insert(full_path);
                    }

                    //println!("  {}", get_parent_path(&asset)?);
                    asset_types.insert(
                        get_full_path(&path, &asset)?, /*sanitized.join(f)*/
                        get_type(&asset),
                    );
                }
            }

            println!("class hierarchy:");
            let trees = build_trees(&hierarchy);
            for tree in &trees {
                tree.print("\t");
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
                .collect::<BTreeSet<_>>();

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

fn pak_path_to_game_path<P: AsRef<PakPath>>(pak_path: P) -> Result<String> {
    let mut components = pak_path.as_ref().components();
    Ok(match components.next() {
        Some(PakPathComponent::Normal("Engine")) => match components.next() {
            Some(PakPathComponent::Normal("Content")) => {
                Some(PakPath::new("/Engine").join(components.as_path()))
            }
            Some(PakPathComponent::Normal("Plugins")) => {
                let mut last = None;
                loop {
                    match components.next() {
                        Some(PakPathComponent::Normal("Content")) => {
                            break last.map(|plugin| {
                                PakPath::new("/").join(plugin).join(components.as_path())
                            })
                        }
                        Some(PakPathComponent::Normal(next)) => {
                            last = Some(next);
                        }
                        _ => break None,
                    }
                }
            }
            _ => None,
        },
        Some(PakPathComponent::Normal(_)) => match components.next() {
            Some(PakPathComponent::Normal("Content")) => {
                Some(PakPath::new("/Game").join(components))
            }
            _ => None,
        },
        _ => None,
    }
    .with_context(|| format!("failed to normalize {}", pak_path.as_ref().as_str()))?
    .to_string())
}

fn get_root_export<R: Read + Seek>(asset: &Asset<R>) -> Result<PackageIndex> {
    for (i, e) in asset.asset_data.exports.iter().enumerate() {
        let base = e.get_base_export();
        if base.outer_index.index == 0 {
            return Ok(PackageIndex::from_export(i as i32).unwrap());
        }
    }
    bail!("no root export")
}

fn get_type<R: Read + Seek>(asset: &Asset<R>) -> Result<String> {
    let root = get_root_export(asset)?;
    let class = asset
        .get_import(
            asset
                .get_export(root)
                .unwrap()
                .get_base_export()
                .class_index,
        )
        .context("missing class import")?;
    Ok(class.object_name.get_owned_content())
}

fn get_full_path<R: Read + Seek>(path: &str, asset: &Asset<R>) -> Result<String> {
    let root = get_root_export(asset)?;
    Ok(asset
        .get_export(root)
        .unwrap()
        .get_base_export()
        .object_name
        .get_content(|c| format!("{path}.{c}")))
}

fn get_parent_path<R: Read + Seek>(asset: &Asset<R>) -> Result<Option<String>> {
    let root = get_root_export(asset)?;
    let export = asset.get_export(root).unwrap().get_base_export();

    let mut import_index = export.super_index;

    if import_index.index == 0 {
        return Ok(None);
    }

    let mut components = vec![];

    while import_index.is_import() {
        let import = asset
            .get_import(import_index)
            .ok_or_else(|| anyhow!("missing import"))?;

        components.insert(0, import.object_name.get_owned_content());

        import_index = import.outer_index;
    }
    Ok(Some(components.join(".")))
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

use std::collections::HashSet;

#[derive(Debug)]
pub struct Node {
    pub id: String,
    pub children: Vec<Node>,
}
impl Node {
    pub fn print(&self, prefix: &str) {
        self.print_node(prefix, &mut vec![])
    }
    fn print_node(&self, prefix: &str, stack: &mut Vec<Edge>) {
        print!("{prefix}");
        for s in &*stack {
            print!("{s}");
        }

        println!("{}", self.id);

        if let Some((last, first)) = self.children.split_last() {
            if let Some(last) = stack.last_mut() {
                if *last == Edge::Corner {
                    *last = Edge::None;
                } else if *last == Edge::T {
                    *last = Edge::Straight;
                }
            }

            {
                stack.push(Edge::T);
                for child in first {
                    child.print_node(prefix, stack);
                }
                stack.pop();
            }

            {
                stack.push(Edge::Corner);
                last.print_node(prefix, stack);
                stack.pop();
            }

            if let Some(last) = stack.last_mut() {
                if *last == Edge::Straight {
                    *last = Edge::T;
                }
            }
        }
    }
}
#[derive(PartialEq)]
enum Edge {
    None,
    Straight,
    Corner,
    T,
}
impl std::fmt::Display for Edge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Edge::None => write!(f, "    "),
            Edge::Straight => write!(f, "│   "),
            Edge::Corner => write!(f, "└── "),
            Edge::T => write!(f, "├── "),
        }
    }
}

fn find_roots(edge_list: &BTreeMap<String, BTreeSet<String>>) -> Vec<&str> {
    let parents = edge_list.keys().collect::<HashSet<_>>();
    let children = edge_list.values().flatten().collect::<HashSet<_>>();

    parents.difference(&children).map(|s| s.as_str()).collect()
}

fn build_node_recursively(id: &str, children_map: &BTreeMap<String, BTreeSet<String>>) -> Node {
    let children = children_map
        .get(id)
        .map(|children| {
            children
                .iter()
                .map(|child_id| build_node_recursively(child_id, children_map))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Node {
        id: id.to_string(),
        children,
    }
}

pub fn build_trees(edge_list: &BTreeMap<String, BTreeSet<String>>) -> Vec<Node> {
    let mut nodes = vec![];
    for root in find_roots(edge_list) {
        nodes.push(build_node_recursively(root, edge_list));
    }
    nodes
}
