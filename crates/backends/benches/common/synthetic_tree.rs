use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct SyntheticTreeSpec {
    pub entries: usize,
    pub seed: u64,
    pub deep_levels: usize,
    pub large_file_bytes: u64,
}

impl SyntheticTreeSpec {
    pub fn flat(entries: usize) -> Self {
        Self {
            entries,
            seed: 0x1d15_cafe,
            deep_levels: 24,
            large_file_bytes: 4 * 1024 * 1024 * 1024,
        }
    }

    pub fn materialize(&self, root: &Path) -> io::Result<SyntheticTree> {
        fs::create_dir_all(root)?;
        self.materialize_flat_entries(root)?;
        self.materialize_deep_chain(root)?;
        Ok(SyntheticTree {
            root: root.to_path_buf(),
            entries: self.entries,
        })
    }

    fn materialize_flat_entries(&self, root: &Path) -> io::Result<()> {
        for index in 0..self.entries {
            let name = self.entry_name(index);
            let path = root.join(name);
            if index.is_multiple_of(31) {
                fs::create_dir(&path)?;
            } else {
                let file = File::create(&path)?;
                if index.is_multiple_of(257) {
                    file.set_len(self.large_file_bytes)?;
                }
            }
        }
        Ok(())
    }

    fn materialize_deep_chain(&self, root: &Path) -> io::Result<()> {
        if self.deep_levels == 0 {
            return Ok(());
        }

        let mut current = root.join("deep-root");
        fs::create_dir(&current)?;
        for level in 0..self.deep_levels {
            current = current.join(format!("level-{level:03}"));
            fs::create_dir(&current)?;
        }
        File::create(current.join("leaf.txt"))?;
        Ok(())
    }

    fn entry_name(&self, index: usize) -> String {
        let shuffled = splitmix64(self.seed ^ index as u64);
        match index % 17 {
            0 => format!("写真-{index:07}-東京-{shuffled:016x}.jpg"),
            1 => format!("報告書-{index:07}-日本語-{shuffled:016x}.pdf"),
            2 => format!("space name {index:07} {shuffled:016x}.txt"),
            _ if index.is_multiple_of(257) => format!("huge-{index:07}-{shuffled:016x}.bin"),
            _ if index.is_multiple_of(31) => format!("dir-{index:07}-{shuffled:016x}"),
            _ => format!("asset-{index:07}-{shuffled:016x}.dat"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SyntheticTree {
    root: PathBuf,
    entries: usize,
}

impl SyntheticTree {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn entries(&self) -> usize {
        self.entries
    }
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut mixed = value;
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^ (mixed >> 31)
}
