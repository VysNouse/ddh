//! # ddh
//!
//! `ddh` is a collection of functions and structs to aid in analysing filesystem directories.

use std::hash::{Hasher};
use std::fs::{self, DirEntry};
use std::io::{Read, BufReader};
use std::path::{PathBuf, Path};
use std::cmp::Ordering;
use serde_derive::{Serialize, Deserialize};
use siphasher::sip128::Hasher128;
use rayon::prelude::*;
use std::sync::mpsc::{Sender, channel};
use std::collections::hash_map::{HashMap, Entry};
use std::io::{Error, ErrorKind};
use nohash_hasher::IntMap;

#[derive(PartialEq)]
enum HashMode{
    Full,
    Partial
}

enum ChannelPackage{
    Success(Fileinfo),
    Fail(PathBuf, std::io::Error),
}

/// Serializable struct containing entries for a specific file.
#[derive(Debug, Serialize, Deserialize)]
pub struct Fileinfo{
    full_hash: Option<u128>,
    partial_hash: Option<u128>,
    file_length: u64,
    file_paths: Vec<PathBuf>,
}

impl Fileinfo{
    pub fn new(hash: Option<u128>, partial_hash: Option<u128>, length: u64, path: PathBuf) -> Self{
        let mut set = Vec::<PathBuf>::new();
        set.push(path);
        Fileinfo{full_hash: hash, partial_hash: partial_hash, file_length: length, file_paths: set}
    }
    pub fn get_length(&self) -> u64{
        self.file_length
    }
    pub fn get_full_hash(&self) -> Option<u128>{
        self.full_hash
    }
    fn set_full_hash(&mut self, hash: Option<u128>) -> (){
        self.full_hash = hash
    }
    fn set_partial_hash(&mut self, hash: Option<u128>) -> (){
        self.partial_hash = hash
    }
    pub fn get_partial_hash(&self) -> Option<u128>{
        self.partial_hash
    }
    pub fn get_candidate_name(&self) -> &str{
        self.file_paths
        .iter()
        .next()
        .unwrap()
        .to_str()
        .unwrap()
        .rsplit("/")
        .next()
        .unwrap()
    }
    pub fn get_paths(&self) -> &Vec<PathBuf>{
        return &self.file_paths
    }

    fn generate_hash(&mut self, mode: HashMode) -> Option<u128>{
        let mut hasher = siphasher::sip128::SipHasher::new();
        match fs::File::open(
            self.file_paths
            .iter()
            .next()
            .expect("Cannot read file path from struct")
            ) {
            Ok(f) => {
                let mut buffer_reader = BufReader::new(f);
                let mut hash_buffer = [0;4096];
                loop {
                    match buffer_reader.read(&mut hash_buffer) {
                        Ok(n) if n>0 => hasher.write(&hash_buffer[0..]),
                        Ok(n) if n==0 => break,
                        Err(_e) => {
                            return None
                        },
                        _ => panic!("Negative length read in hashing"),
                        }
                    if mode == HashMode::Partial{
                        return Some(hasher.finish128().into());
                    }
                }
                return Some(hasher.finish128().into());
            }
            Err(_e) => {
                return None
            }
        }
    }
}

impl PartialEq for Fileinfo{
    fn eq(&self, other: &Fileinfo) -> bool {
        (self.file_length==other.file_length)&&
        (self.partial_hash==other.partial_hash)&&
        (self.full_hash==other.full_hash)
    }
}
impl Eq for Fileinfo{}

impl PartialOrd for Fileinfo{
    fn partial_cmp(&self, other: &Fileinfo) -> Option<Ordering>{
         if self.full_hash.is_some() && other.full_hash.is_some(){
            Some(self.full_hash.cmp(&other.full_hash))
        } else if self.partial_hash.is_some() && other.partial_hash.is_some(){
            Some(self.partial_hash.cmp(&other.partial_hash))
        } else {
            Some(self.file_length.cmp(&other.file_length))
        }
    }
}

impl Ord for Fileinfo{
    fn cmp(&self, other: &Fileinfo) -> Ordering {
        if self.full_hash.is_some() && other.full_hash.is_some(){
            self.full_hash.cmp(&other.full_hash)
        } else if self.partial_hash.is_some() && other.partial_hash.is_some(){
            self.partial_hash.cmp(&other.partial_hash)
        } else {
            self.file_length.cmp(&other.file_length)
        }
    }
}

/// Constructs a list of unique files from a list of directories.
///
/// # Examples
/// ```
/// let directories = vec!["/home/jon", "/home/doe"];
/// let (files, errors) = ddh::deduplicate_dirs(directories);
/// ```
pub fn deduplicate_dirs(search_dirs: Vec<&str>) -> (Vec<Fileinfo>, Vec<(PathBuf, std::io::Error)>){
    let (sender, receiver) = channel();
    search_dirs.par_iter().for_each_with(sender, |s, search_dir| {
            traverse_and_spawn(Path::new(&search_dir), s.clone());
    });
    let mut files_of_lengths: IntMap<u64, Vec<Fileinfo>> = IntMap::default();
    let mut errors = Vec::new();
    for pkg in receiver.iter(){
        match pkg{
            ChannelPackage::Success(entry) => {
                match files_of_lengths.entry(entry.get_length()) {
                    Entry::Vacant(e) => { e.insert(vec![entry]); },
                    Entry::Occupied(mut e) => { e.get_mut().push(entry); }
                }
            },
            ChannelPackage::Fail(entry, error) => {
                errors.push((entry, error));
            },
        }
    }
    let complete_files: Vec<Fileinfo> = files_of_lengths.into_par_iter()
        .map(|x| differentiate_and_consolidate(x.0, x.1))
        .flatten()
        .collect();
    (complete_files, errors)
}

fn traverse_and_spawn(current_path: &Path, sender: Sender<ChannelPackage>) -> (){
    let tmp_md = fs::metadata(current_path);
    if tmp_md.is_err() {
        sender.send(
        ChannelPackage::Fail(current_path.to_path_buf(), tmp_md.err().unwrap())
        ).expect("Error sending new ChannelPackage::Fail");
        return
    }
    let current_path_metadata = tmp_md.unwrap();

    if current_path_metadata.file_type().is_symlink(){
        sender.send(
        ChannelPackage::Fail(current_path.to_path_buf(), Error::new(ErrorKind::Other, "Path is symlink"))
        ).expect("Error sending new ChannelPackage::Fail");
        return
    }

    if current_path_metadata.file_type().is_file(){
            sender.send(ChannelPackage::Success(
            Fileinfo::new(
                None,
                None,
                current_path.metadata().expect("Error reading path length").len(),
                current_path.to_path_buf()
                ))
            ).expect("Error sending new ChannelPackage::Success");
        return
    }

    if current_path_metadata.file_type().is_dir(){
        match fs::read_dir(current_path) {
                Ok(read_dir_results) => {
                    let good_entries: Vec<_> = read_dir_results
                    .filter(|x| x.is_ok())
                    .map(|x| x.unwrap())
                    .collect();
                    let (files, dirs): (Vec<&DirEntry>, Vec<&DirEntry>) = good_entries.par_iter().partition(|&x|
                        x.path()
                        .as_path()
                        .symlink_metadata()
                        .expect("Error reading Symlink Metadata")
                        .file_type()
                        .is_file()
                        );
                    files.par_iter().for_each_with(sender.clone(), |sender, x|
                        sender.send(ChannelPackage::Success(
                            Fileinfo::new(
                                None,
                                None,
                                x.metadata().expect("Error reading path length").len(),
                                x.path()))
                                ).expect("Error sending new ChannelPackage::Success")
                            );
                    dirs.into_par_iter()
                    .for_each_with(sender, |sender, x| {
                            traverse_and_spawn(x.path().as_path(), sender.clone());
                    })
                },
                Err(e) => {
                    sender.send(
                        ChannelPackage::Fail(current_path.to_path_buf(), e)
                        ).expect("Error sending new ChannelPackage::Fail");
                },
            }
    } else {}
}

fn differentiate_and_consolidate(file_length: u64, mut files: Vec<Fileinfo>) -> Vec<Fileinfo>{
    if file_length==0{
        return files
    }
    if files.len()<=0{
        panic!("Invalid length vector");
    }
    match files.len(){
        1 => return files,
        n if n>1 => {
            files.par_iter_mut().for_each(|file_ref| {
                let hash = file_ref.generate_hash(HashMode::Partial);
                file_ref.set_partial_hash(hash);
            });
            if file_length<=4096{
                files.par_iter_mut().for_each(|x|{
                    let hash = x.get_partial_hash().unwrap();
                    x.set_full_hash(Some(hash)) ;
                });
                return dedupe(files)
            }
            let mut partial_hashes: HashMap<u128, u64> = HashMap::new();
            for file in files.iter(){
                match partial_hashes.entry(file.get_partial_hash().unwrap()){
                    Entry::Vacant(e) => { e.insert(0); },
                    Entry::Occupied(mut e) => {*e.get_mut()+=1;}
                }
            }
            let dedupe_hashes: Vec<_> = partial_hashes
                .into_iter()
                .filter(|x| x.1>0)
                .map(|y| y.0)
                .collect();
            files.par_iter_mut().for_each(|x|
                if dedupe_hashes.contains(&x.get_partial_hash().unwrap()){
                    let hash = x.generate_hash(HashMode::Full);
                    x.set_full_hash(hash);
                }
            );
        },
        _ => {panic!("Somehow a vector of negative length was created. Please report this as a bug");}
    }
    dedupe(files)
}

fn dedupe(mut files: Vec<Fileinfo>) -> Vec<Fileinfo>{
    let mut cache: HashMap<(Option<u128>, Option<u128>), _> = HashMap::new();
    for file in files.iter_mut(){
        match cache.entry((file.get_partial_hash(), file.get_full_hash())){
                    Entry::Vacant(e) => {
                        e.insert(file);
                    },
                    Entry::Occupied(mut e) => {
                        e.get_mut()
                        .file_paths
                        .extend(file.file_paths.drain(..));
                    }
                }
    }
    files.retain(|x| x.get_paths().len()>0);
    files
}
