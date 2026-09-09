//! Per-scope, bounded repository map: docs/design/context.md#repository-map.
use anyhow::{Context, Result};
use std::{collections::BTreeMap, ffi::OsStr, fs, path::{Path, PathBuf}, process::Command, time::UNIX_EPOCH};

pub const MAX_REPO_MAP_BYTES: usize = 8 * 1024;
const MAX_FILES: usize = 20_000;
const MAX_SYMBOL_FILE_BYTES: u64 = 256 * 1024;
const GENERATED_DIRS: &[&str] = &[".git",".harness","target","node_modules","dist","build","coverage","vendor",".next",".venv","venv","__pycache__"];

#[derive(Clone, Debug)]
pub struct RepoMap { pub id:String, pub text:String, pub refreshed:bool }

fn denied(rel:&Path)->bool {
    rel.components().any(|component| {
        let name=component.as_os_str().to_string_lossy();
        GENERATED_DIRS.contains(&name.as_ref()) || crate::tools::paths::is_denied_name(&name)
    }) || rel.file_name().and_then(OsStr::to_str).is_some_and(|name|
        name.ends_with(".min.js") || name.ends_with(".map") || name.ends_with(".pyc") || name.ends_with(".lock"))
}

fn git_files(root:&Path)->Option<Vec<PathBuf>> {
    let output=Command::new("git").args(["-C",root.to_str()?,"ls-files","-co","--exclude-standard","-z","--"])
        .output().ok()?;
    if !output.status.success(){return None;}
    let mut files=output.stdout.split(|b|*b==0).filter(|p|!p.is_empty())
        .filter_map(|raw|std::str::from_utf8(raw).ok()).map(PathBuf::from)
        .filter(|rel|!denied(rel))
        .filter(|rel|fs::symlink_metadata(root.join(rel)).ok().is_some_and(|meta|meta.file_type().is_file()))
        .collect::<Vec<_>>();
    files.sort();files.dedup();files.truncate(MAX_FILES);Some(files)
}

fn walk(dir:&Path,root:&Path,out:&mut Vec<PathBuf>)->Result<()> {
    if out.len()>=MAX_FILES{return Ok(());}
    let mut entries=fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e|e.file_name());
    for entry in entries {
        if out.len()>=MAX_FILES{break;}
        let path=entry.path();
        let rel=path.strip_prefix(root).unwrap_or(&path);
        if denied(rel){continue;}
        let kind=entry.file_type()?;
        if kind.is_symlink(){continue;}
        if kind.is_dir(){walk(&path,root,out)?;}else if kind.is_file(){out.push(rel.to_path_buf());}
    }
    Ok(())
}

fn files(root:&Path)->Result<Vec<PathBuf>> {
    if let Some(files)=git_files(root){return Ok(files);}
    let mut found=Vec::new();walk(root,root,&mut found)?;found.sort();Ok(found)
}

fn signature(root:&Path,files:&[PathBuf])->Result<String> {
    let mut state=String::new();
    for rel in files {
        let meta=fs::symlink_metadata(root.join(rel))?;
        if !meta.is_file(){continue;}
        let modified=meta.modified().ok().and_then(|v|v.duration_since(UNIX_EPOCH).ok()).map(|v|v.as_nanos()).unwrap_or(0);
        state.push_str(&format!("{}\0{}\0{}\n",rel.to_string_lossy(),meta.len(),modified));
    }
    Ok(crate::safety::fingerprint(&state))
}

fn symbol_name(line:&str)->Option<String> {
    if line.len()>500 || line.starts_with(char::is_whitespace){return None;}
    let trimmed=line.trim();
    let prefixes=["pub async fn ","pub fn ","async fn ","fn ","pub struct ","struct ","pub enum ","enum ","pub trait ","trait ",
        "def ","async def ","class ","function ","export function ","export class ","export const ","func ","type ","interface "];
    for prefix in prefixes {
        if let Some(rest)=trimmed.strip_prefix(prefix){
            let name=rest.split(|c:char|c.is_whitespace() || matches!(c,'('|'<'|'{'|'='|':'|';')).next().unwrap_or("");
            if !name.is_empty(){return Some(name.to_string());}
        }
    }
    if let Some(heading)=trimmed.strip_prefix("# ").or_else(||trimmed.strip_prefix("## ")) {
        let heading=heading.trim();if !heading.is_empty(){return Some(format!("§ {heading}"));}
    }
    None
}

fn fallback_symbols(root:&Path,files:&[PathBuf])->BTreeMap<PathBuf,Vec<String>> {
    let mut result=BTreeMap::new();
    for rel in files {
        let path=root.join(rel);
        if fs::metadata(&path).ok().is_none_or(|m|m.len()>MAX_SYMBOL_FILE_BYTES){continue;}
        let Ok(text)=fs::read_to_string(path) else {continue};
        let mut symbols=Vec::new();
        for line in text.lines() {
            if let Some(name)=symbol_name(line){if !symbols.contains(&name){symbols.push(name);}}
            if symbols.len()==24{break;}
        }
        if !symbols.is_empty(){result.insert(rel.clone(),symbols);}
    }
    result
}

/// Universal/Exuberant ctags is preferred when installed. A failed or unfamiliar ctags binary is
/// harmless: deterministic language-neutral extraction remains the fallback.
fn ctags_symbols(root:&Path,files:&[PathBuf])->Option<BTreeMap<PathBuf,Vec<String>>> {
    if files.is_empty() || files.len()>2_000{return None;}
    let mut command=Command::new("ctags");
    command.current_dir(root).args(["-x","--sort=no"]);
    for rel in files {command.arg(rel);}
    let output=command.output().ok()?;
    if !output.status.success(){return None;}
    let text=String::from_utf8(output.stdout).ok()?;
    let mut result:BTreeMap<PathBuf,Vec<String>>=BTreeMap::new();
    for line in text.lines() {
        let fields=line.split_whitespace().collect::<Vec<_>>();
        if fields.len()<4{continue;}
        let rel=PathBuf::from(fields[3]);
        if !files.binary_search(&rel).is_ok(){continue;}
        let symbols=result.entry(rel).or_default();
        if symbols.len()<24 && !symbols.iter().any(|v|v==fields[0]){symbols.push(fields[0].to_string());}
    }
    (!result.is_empty()).then_some(result)
}

fn bounded(signature:&str,files:&[PathBuf],symbols:&BTreeMap<PathBuf,Vec<String>>)->String {
    let mut out=format!("# harness repo-map v1 signature:{signature}\n");
    for rel in files {
        let mut line=rel.to_string_lossy().replace('\\',"/");
        if let Some(names)=symbols.get(rel){line.push_str(" :: ");line.push_str(&names.join(", "));}
        line.push('\n');
        if out.len()+line.len()>MAX_REPO_MAP_BYTES {
            let marker="… repository map truncated at 8192 bytes\n";
            while out.len()+marker.len()>MAX_REPO_MAP_BYTES {if out.pop().is_none(){break;}}
            out.push_str(marker);break;
        }
        out.push_str(&line);
    }
    debug_assert!(out.len()<=MAX_REPO_MAP_BYTES);
    out
}

/// Recompute only when path/size/mtime metadata changes. The cache write is atomic and best-effort:
/// a read-only project still gets an in-memory map for the current turn.
pub fn load_or_refresh(root:&Path)->Result<RepoMap> {
    let files=files(root).context("enumerate repository files")?;
    let signature=signature(root,&files)?;
    let id=format!("repo-map:{signature}");
    let cache=root.join(".harness/repo_map.txt");
    if let Ok(text)=fs::read_to_string(&cache) {
        if text.lines().next().is_some_and(|line|line.ends_with(&format!("signature:{signature}"))) && text.len()<=MAX_REPO_MAP_BYTES {
            return Ok(RepoMap{id,text,refreshed:false});
        }
    }
    let symbols=ctags_symbols(root,&files).unwrap_or_else(||fallback_symbols(root,&files));
    let text=bounded(&signature,&files,&symbols);
    if let Some(parent)=cache.parent() {
        if fs::create_dir_all(parent).is_ok() {
            let temp=parent.join("repo_map.tmp");
            if fs::write(&temp,&text).is_ok(){let _=fs::rename(temp,&cache);}
        }
    }
    Ok(RepoMap{id,text,refreshed:true})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn project()->PathBuf{
        let root=std::env::temp_dir().join(format!("harness-map-{}",crate::storage::uid()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"),"pub struct Engine;\nfn start() {}\n").unwrap();
        fs::write(root.join("README.md"),"# Project\n").unwrap();
        fs::write(root.join(".env"),"SECRET=never-index\n").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();fs::write(root.join("target/generated.rs"),"fn hidden(){}\n").unwrap();
        fs::canonicalize(root).unwrap()
    }
    #[test] fn builds_a_bounded_secret_free_symbol_map_and_caches_it(){
        let root=project();
        let first=load_or_refresh(&root).unwrap();
        assert!(first.refreshed && first.text.len()<=MAX_REPO_MAP_BYTES);
        assert!(first.text.contains("src/lib.rs :: Engine, start"));
        assert!(first.text.contains("README.md :: § Project"));
        assert!(!first.text.contains(".env") && !first.text.contains("generated.rs"));
        let second=load_or_refresh(&root).unwrap();assert!(!second.refreshed);assert_eq!(first.text,second.text);
        fs::write(root.join("src/lib.rs"),"pub struct Engine;\nfn restart() {}\n").unwrap();
        let third=load_or_refresh(&root).unwrap();assert!(third.refreshed && third.text.contains("restart"));
        fs::remove_dir_all(root).ok();
    }
    #[test] fn truncates_only_at_utf8_boundaries(){
        let files=(0..2000).map(|i|PathBuf::from(format!("資料/{i:04}-{}.rs","長".repeat(8)))).collect::<Vec<_>>();
        let text=bounded("digest",&files,&BTreeMap::new());
        assert!(text.len()<=MAX_REPO_MAP_BYTES && std::str::from_utf8(text.as_bytes()).is_ok());
        assert!(text.contains("truncated"));
    }
    #[cfg(unix)]
    #[test] fn git_discovery_rejects_tracked_or_untracked_symlinks(){
        let root=project();
        assert!(Command::new("git").arg("init").arg("-q").arg(&root).status().unwrap().success());
        std::os::unix::fs::symlink(root.join(".env"),root.join("linked.rs")).unwrap();
        let map=load_or_refresh(&root).unwrap();
        assert!(!map.text.contains("linked.rs") && !map.text.contains("never-index"));
        fs::remove_dir_all(root).ok();
    }
}
