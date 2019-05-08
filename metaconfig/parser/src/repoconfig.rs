// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

//! Contains structures describing configuration of the entire repo. Those structures are
//! deserialized from TOML files from metaconfig repo

use serde_derive::Deserialize;
use std::{
    collections::HashMap,
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    str,
    time::Duration,
};

use crate::errors::*;
use bookmarks::Bookmark;
use failure_ext::ResultExt;
use metaconfig_types::{
    BlobstoreId, BookmarkOrRegex, BookmarkParams, Bundle2ReplayParams, CacheWarmupParams,
    CommonConfig, GlusterArgs, HookBypass, HookConfig, HookManagerParams, HookParams, HookType,
    LfsParams, ManifoldArgs, MysqlBlobstoreArgs, PushrebaseParams, RemoteBlobstoreArgs, RepoConfig,
    RepoReadOnly, RepoType, ShardedFilenodesParams, WhitelistEntry,
};
use regex::Regex;
use toml;

/// Configuration of a metaconfig repository
#[derive(Debug, Eq, PartialEq)]
pub struct MetaConfig {}

/// Holds configuration all configuration that was read from metaconfig repository's manifest.
#[derive(Debug, PartialEq)]
pub struct RepoConfigs {
    /// Config for the config repository
    pub metaconfig: MetaConfig,
    /// Configs for all other repositories
    pub repos: HashMap<String, RepoConfig>,
    /// Common configs for all repos
    pub common: CommonConfig,
}

impl RepoConfigs {
    /// Read repo configs
    pub fn read_configs<P: AsRef<Path>>(config_path: P) -> Result<Self> {
        let repos_dir = config_path.as_ref().join("repos");
        if !repos_dir.is_dir() {
            return Err(
                ErrorKind::InvalidFileStructure("expected 'repos' directory".into()).into(),
            );
        }
        let mut repo_configs = HashMap::new();
        for entry in repos_dir.read_dir()? {
            let entry = entry?;
            let dir_path = entry.path();
            if dir_path.is_dir() {
                let (name, config) =
                    RepoConfigs::read_single_repo_config(&dir_path, config_path.as_ref())
                        .context(format!("while opening config for {:?} repo", dir_path))?;
                repo_configs.insert(name, config);
            }
        }

        let common_dir = config_path.as_ref().join("common");
        let maybe_common_config = if common_dir.is_dir() {
            Self::read_common_config(&common_dir)?
        } else {
            None
        };

        let common = maybe_common_config.unwrap_or(Default::default());
        Ok(Self {
            metaconfig: MetaConfig {},
            repos: repo_configs,
            common,
        })
    }

    fn read_common_config(common_dir: &PathBuf) -> Result<Option<CommonConfig>> {
        for entry in common_dir.read_dir()? {
            let entry = entry?;
            if entry.file_name() == "common.toml" {
                let path = entry.path();
                if !path.is_file() {
                    return Err(ErrorKind::InvalidFileStructure(
                        "common/common.toml should be a file!".into(),
                    )
                    .into());
                }

                let content = fs::read(path)?;
                let raw_config = toml::from_slice::<RawCommonConfig>(&content)?;
                let mut tiers_num = 0;
                let whitelisted_entries: Result<Vec<_>> = raw_config
                    .whitelist_entry
                    .unwrap_or(vec![])
                    .into_iter()
                    .map(|whitelist_entry| {
                        let has_tier = whitelist_entry.tier.is_some();
                        let has_identity = {
                            if whitelist_entry.identity_data.is_none()
                                ^ whitelist_entry.identity_type.is_none()
                            {
                                return Err(ErrorKind::InvalidFileStructure(
                                    "identity type and data must be specified".into(),
                                )
                                .into());
                            }

                            whitelist_entry.identity_type.is_some()
                        };

                        if has_tier && has_identity {
                            return Err(ErrorKind::InvalidFileStructure(
                                "tier and identity cannot be both specified".into(),
                            )
                            .into());
                        }

                        if !has_tier && !has_identity {
                            return Err(ErrorKind::InvalidFileStructure(
                                "tier or identity must be specified".into(),
                            )
                            .into());
                        }

                        if whitelist_entry.tier.is_some() {
                            tiers_num += 1;
                            Ok(WhitelistEntry::Tier(whitelist_entry.tier.unwrap()))
                        } else {
                            let identity_type = whitelist_entry.identity_type.unwrap();

                            Ok(WhitelistEntry::HardcodedIdentity {
                                ty: identity_type,
                                data: whitelist_entry.identity_data.unwrap(),
                            })
                        }
                    })
                    .collect();

                if tiers_num > 1 {
                    return Err(
                        ErrorKind::InvalidFileStructure("only one tier is allowed".into()).into(),
                    );
                }
                return Ok(Some(CommonConfig {
                    security_config: whitelisted_entries?,
                }));
            }
        }
        Ok(None)
    }

    fn read_single_repo_config(
        repo_config_path: &Path,
        config_root_path: &Path,
    ) -> Result<(String, RepoConfig)> {
        let reponame = repo_config_path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                let e: Error = ErrorKind::InvalidFileStructure(format!(
                    "invalid repo path {:?}",
                    repo_config_path
                ))
                .into();
                e
            })?;
        let reponame = reponame.to_string();

        let config_file = repo_config_path.join("server.toml");
        if !config_file.is_file() {
            return Err(ErrorKind::InvalidFileStructure(format!(
                "expected file server.toml in {}",
                repo_config_path.to_string_lossy()
            ))
            .into());
        }

        let raw_config = toml::from_slice::<RawRepoConfig>(&fs::read(&config_file)?)?;

        let hooks = raw_config.hooks.clone();
        // Easier to deal with empty vector than Option
        let hooks = hooks.unwrap_or(Vec::new());

        let mut all_hook_params = vec![];
        for raw_hook_config in hooks {
            let config = HookConfig {
                bypass: RepoConfigs::get_bypass(raw_hook_config.clone())?,
                strings: raw_hook_config.config_strings.unwrap_or_default(),
                ints: raw_hook_config.config_ints.unwrap_or_default(),
            };

            let hook_params = if raw_hook_config.name.starts_with("rust:") {
                // No need to load lua code for rust hook
                HookParams {
                    name: raw_hook_config.name,
                    code: None,
                    hook_type: raw_hook_config.hook_type,
                    config,
                }
            } else {
                let path = raw_hook_config.path.clone();
                let path = match path {
                    Some(path) => path,
                    None => {
                        return Err(ErrorKind::MissingPath().into());
                    }
                };
                let relative_prefix = "./";
                let is_relative = path.starts_with(relative_prefix);
                let path_adjusted = if is_relative {
                    let s: String = path.chars().skip(relative_prefix.len()).collect();
                    repo_config_path.join(s)
                } else {
                    config_root_path.join(path)
                };

                let contents = fs::read(&path_adjusted)
                    .context(format!("while reading hook {:?}", path_adjusted))?;
                let code = str::from_utf8(&contents)?;
                let code = code.to_string();
                HookParams {
                    name: raw_hook_config.name,
                    code: Some(code),
                    hook_type: raw_hook_config.hook_type,
                    config,
                }
            };

            all_hook_params.push(hook_params);
        }
        Ok((
            reponame,
            RepoConfigs::convert_conf(raw_config, all_hook_params)?,
        ))
    }

    fn get_bypass(raw_hook_config: RawHookConfig) -> Result<Option<HookBypass>> {
        let bypass_commit_message = raw_hook_config
            .bypass_commit_string
            .map(|s| HookBypass::CommitMessage(s));

        let bypass_pushvar = raw_hook_config.bypass_pushvar.and_then(|s| {
            let pushvar: Vec<_> = s.split('=').map(|val| val.to_string()).collect();
            if pushvar.len() != 2 {
                return Some(Err(ErrorKind::InvalidPushvar(s).into()));
            }
            Some(Ok((
                pushvar.get(0).unwrap().clone(),
                pushvar.get(1).unwrap().clone(),
            )))
        });
        let bypass_pushvar = match bypass_pushvar {
            Some(Err(err)) => {
                return Err(err);
            }
            Some(Ok((name, value))) => Some(HookBypass::Pushvar { name, value }),
            None => None,
        };

        if bypass_commit_message.is_some() && bypass_pushvar.is_some() {
            return Err(ErrorKind::TooManyBypassOptions(raw_hook_config.name).into());
        }
        let bypass = bypass_commit_message.or(bypass_pushvar);

        Ok(bypass)
    }

    fn convert_conf(this: RawRepoConfig, hooks: Vec<HookParams>) -> Result<RepoConfig> {
        fn get_path(config: &RawRepoConfig) -> ::std::result::Result<PathBuf, ErrorKind> {
            config.path.clone().ok_or_else(|| {
                ErrorKind::InvalidConfig(format!(
                    "No path provided for {:#?} type of repo",
                    config.repotype
                ))
            })
        }

        let repotype = match this.repotype {
            RawRepoType::Files => RepoType::BlobFiles(get_path(&this)?),
            RawRepoType::BlobRocks => RepoType::BlobRocks(get_path(&this)?),
            RawRepoType::BlobSqlite => RepoType::BlobSqlite(get_path(&this)?),
            RawRepoType::BlobRemote => {
                let remote_blobstores = this.remote_blobstore.ok_or(ErrorKind::InvalidConfig(
                    "remote blobstores must be specified".into(),
                ))?;
                let db_address = this.db_address.ok_or(ErrorKind::InvalidConfig(
                    "xdb tier was not specified".into(),
                ))?;

                let write_lock_db_address = this.write_lock_db_address;

                let mut blobstores = HashMap::new();
                for blobstore in remote_blobstores {
                    let args = match blobstore.blobstore_type {
                        RawBlobstoreType::Manifold => {
                            let manifold_bucket =
                                blobstore.manifold_bucket.ok_or(ErrorKind::InvalidConfig(
                                    "manifold bucket must be specified".into(),
                                ))?;
                            let manifold_args = ManifoldArgs {
                                bucket: manifold_bucket,
                                prefix: blobstore.manifold_prefix.unwrap_or("".into()),
                            };
                            RemoteBlobstoreArgs::Manifold(manifold_args)
                        }
                        RawBlobstoreType::Gluster => {
                            let tier = blobstore.gluster_tier.ok_or(ErrorKind::InvalidConfig(
                                "gluster tier must be specified".into(),
                            ))?;
                            let export = blobstore.gluster_export.ok_or(
                                ErrorKind::InvalidConfig("gluster bucket must be specified".into()),
                            )?;
                            let basepath =
                                blobstore.gluster_basepath.ok_or(ErrorKind::InvalidConfig(
                                    "gluster basepath must be specified".into(),
                                ))?;
                            RemoteBlobstoreArgs::Gluster(GlusterArgs {
                                tier,
                                export,
                                basepath,
                            })
                        }
                        RawBlobstoreType::Mysql => {
                            let shardmap = blobstore.mysql_shardmap.ok_or(
                                ErrorKind::InvalidConfig("mysql shardmap must be specified".into()),
                            )?;
                            let shard_num = blobstore
                                .mysql_shard_num
                                .and_then(|shard_num| {
                                    if shard_num > 0 {
                                        NonZeroUsize::new(shard_num as usize)
                                    } else {
                                        None
                                    }
                                })
                                .ok_or(ErrorKind::InvalidConfig(
                                    "mysql shard num must be specified and an interger larger \
                                     than 0"
                                        .into(),
                                ))?;
                            RemoteBlobstoreArgs::Mysql(MysqlBlobstoreArgs {
                                shardmap,
                                shard_num,
                            })
                        }
                    };
                    if blobstores.insert(blobstore.blobstore_id, args).is_some() {
                        return Err(ErrorKind::InvalidConfig(
                            "blobstore identifiers are not unique".into(),
                        )
                        .into());
                    }
                }

                let blobstores_args = if blobstores.len() == 1 {
                    let (_, args) = blobstores.into_iter().next().unwrap();
                    args
                } else {
                    RemoteBlobstoreArgs::Multiplexed {
                        scuba_table: this.blobstore_scuba_table,
                        blobstores,
                    }
                };

                let sharded_filenodes: Result<Option<ShardedFilenodesParams>> = this
                    .sharded_filenodes
                    .map(|params| {
                        let RawShardedFilenodesParams {
                            shard_map,
                            shard_num,
                        } = params;
                        let err =
                            ErrorKind::InvalidConfig("filenodes shard_num must be > 0".into());
                        let shard_num: Result<NonZeroUsize> =
                            NonZeroUsize::new(shard_num).ok_or(err.into());
                        Ok(ShardedFilenodesParams {
                            shard_map,
                            shard_num: shard_num?,
                        })
                    })
                    .transpose();

                RepoType::BlobRemote {
                    blobstores_args,
                    db_address,
                    sharded_filenodes: sharded_filenodes?,
                    write_lock_db_address,
                }
            }
        };

        let enabled = this.enabled.unwrap_or(true);
        let generation_cache_size = this.generation_cache_size.unwrap_or(10 * 1024 * 1024);
        let repoid = this.repoid;
        let scuba_table = this.scuba_table;
        let wireproto_scribe_category = this.wireproto_scribe_category;
        let cache_warmup = this.cache_warmup.map(|cache_warmup| CacheWarmupParams {
            bookmark: Bookmark::new(cache_warmup.bookmark).expect("bookmark name must be ascii"),
            commit_limit: cache_warmup.commit_limit.unwrap_or(200000),
        });
        let hook_manager_params = this.hook_manager_params.map(|params| HookManagerParams {
            entrylimit: params.entrylimit,
            weightlimit: params.weightlimit,
            disable_acl_checker: params.disable_acl_checker,
        });
        let bookmarks = match this.bookmarks {
            Some(bookmarks) => {
                let mut bookmark_params = Vec::new();
                for bookmark in bookmarks {
                    let bookmark_or_regex = match (bookmark.regex, bookmark.name) {
                        (None, Some(name)) => {
                            BookmarkOrRegex::Bookmark(Bookmark::new(name).unwrap())
                        }
                        (Some(regex), None) => BookmarkOrRegex::Regex(regex.0),
                        _ => {
                            return Err(ErrorKind::InvalidConfig(
                                "bookmark's params need to specify regex xor name".into(),
                            )
                            .into());
                        }
                    };

                    let only_fast_forward = bookmark.only_fast_forward.unwrap_or(false);
                    let allowed_users = bookmark.allowed_users.map(|re| re.0);

                    bookmark_params.push(BookmarkParams {
                        bookmark: bookmark_or_regex,
                        hooks: match bookmark.hooks {
                            Some(hooks) => hooks.into_iter().map(|rbmh| rbmh.hook_name).collect(),
                            None => vec![],
                        },
                        only_fast_forward,
                        allowed_users,
                    });
                }
                bookmark_params
            }
            None => vec![],
        };
        let bookmarks_cache_ttl = this.bookmarks_cache_ttl.map(Duration::from_millis);

        let pushrebase = this
            .pushrebase
            .map(|raw| {
                let default = PushrebaseParams::default();
                PushrebaseParams {
                    rewritedates: raw.rewritedates.unwrap_or(default.rewritedates),
                    recursion_limit: raw.recursion_limit.unwrap_or(default.recursion_limit),
                    commit_scribe_category: raw.commit_scribe_category,
                    block_merges: raw.block_merges.unwrap_or(default.block_merges),
                    forbid_p2_root_rebases: raw
                        .forbid_p2_root_rebases
                        .unwrap_or(default.forbid_p2_root_rebases),
                    casefolding_check: raw.casefolding_check.unwrap_or(default.casefolding_check),
                    emit_obsmarkers: raw.emit_obsmarkers.unwrap_or(default.emit_obsmarkers),
                }
            })
            .unwrap_or_default();

        let bundle2_replay_params = this
            .bundle2_replay_params
            .map(|raw| Bundle2ReplayParams {
                preserve_raw_bundle2: raw.preserve_raw_bundle2.unwrap_or(false),
            })
            .unwrap_or_default();

        let lfs = match this.lfs {
            Some(lfs_params) => LfsParams {
                threshold: lfs_params.threshold,
            },
            None => LfsParams { threshold: None },
        };

        let hash_validation_percentage = this.hash_validation_percentage.unwrap_or(0);

        let readonly = if this.readonly.unwrap_or(false) {
            RepoReadOnly::ReadOnly("Set by config option".to_string())
        } else {
            RepoReadOnly::ReadWrite
        };

        let skiplist_index_blobstore_key = this.skiplist_index_blobstore_key;
        Ok(RepoConfig {
            enabled,
            repotype,
            generation_cache_size,
            repoid,
            scuba_table,
            cache_warmup,
            hook_manager_params,
            bookmarks,
            bookmarks_cache_ttl,
            hooks,
            pushrebase,
            lfs,
            wireproto_scribe_category,
            hash_validation_percentage,
            readonly,
            skiplist_index_blobstore_key,
            bundle2_replay_params,
        })
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct RawCommonConfig {
    whitelist_entry: Option<Vec<RawWhitelistEntry>>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct RawWhitelistEntry {
    tier: Option<String>,
    identity_data: Option<String>,
    identity_type: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct RawRepoConfig {
    path: Option<PathBuf>,
    repotype: RawRepoType,
    enabled: Option<bool>,
    generation_cache_size: Option<usize>,
    repoid: i32,
    db_address: Option<String>,
    write_lock_db_address: Option<String>,
    scuba_table: Option<String>,
    blobstore_scuba_table: Option<String>,
    delay_mean: Option<u64>,
    delay_stddev: Option<u64>,
    cache_warmup: Option<RawCacheWarmupConfig>,
    bookmarks: Option<Vec<RawBookmarkConfig>>,
    bookmarks_cache_ttl: Option<u64>,
    hooks: Option<Vec<RawHookConfig>>,
    pushrebase: Option<RawPushrebaseParams>,
    lfs: Option<RawLfsParams>,
    wireproto_scribe_category: Option<String>,
    hash_validation_percentage: Option<usize>,
    readonly: Option<bool>,
    hook_manager_params: Option<HookManagerParams>,
    skiplist_index_blobstore_key: Option<String>,
    remote_blobstore: Option<Vec<RawRemoteBlobstoreConfig>>,
    bundle2_replay_params: Option<RawBundle2ReplayParams>,
    sharded_filenodes: Option<RawShardedFilenodesParams>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct RawCacheWarmupConfig {
    bookmark: String,
    commit_limit: Option<usize>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct RawHookManagerParams {
    entrylimit: usize,
    weightlimit: usize,
}

/// This structure helps to resolve an issue that when using serde_regex on Option<Regex> parsing
/// the toml file fails when the "regex" field is not provided. It works as expected when the
/// indirect Option<RawRegex> is used.
#[derive(Debug, Deserialize, Clone)]
struct RawRegex(#[serde(with = "serde_regex")] Regex);

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct RawBookmarkConfig {
    /// Either the regex or the name should be provided, not both
    regex: Option<RawRegex>,
    name: Option<String>,
    hooks: Option<Vec<RawBookmarkHook>>,
    // Are non fastforward moves allowed for this bookmark
    only_fast_forward: Option<bool>,
    /// Only users matching this pattern will be allowed to move this bookmark
    allowed_users: Option<RawRegex>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct RawBookmarkHook {
    hook_name: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct RawHookConfig {
    name: String,
    path: Option<String>,
    hook_type: HookType,
    bypass_commit_string: Option<String>,
    bypass_pushvar: Option<String>,
    config_strings: Option<HashMap<String, String>>,
    config_ints: Option<HashMap<String, i32>>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct RawRemoteBlobstoreConfig {
    blobstore_type: RawBlobstoreType,
    blobstore_id: BlobstoreId,
    // required manifold arguments
    manifold_bucket: Option<String>,
    manifold_prefix: Option<String>,
    // required gluster arguments
    gluster_tier: Option<String>,
    gluster_export: Option<String>,
    gluster_basepath: Option<String>,
    // required mysql arguments
    mysql_shardmap: Option<String>,
    mysql_shard_num: Option<i32>,
}

/// Types of repositories supported
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
enum RawRepoType {
    #[serde(rename = "blob:files")]
    Files,
    #[serde(rename = "blob:rocks")]
    BlobRocks,
    #[serde(rename = "blob:sqlite")]
    BlobSqlite,
    #[serde(rename = "blob:remote")]
    BlobRemote,
}

/// Types of blobstores supported
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
enum RawBlobstoreType {
    #[serde(rename = "manifold")]
    Manifold,
    #[serde(rename = "gluster")]
    Gluster,
    #[serde(rename = "mysql")]
    Mysql,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPushrebaseParams {
    rewritedates: Option<bool>,
    recursion_limit: Option<usize>,
    commit_scribe_category: Option<String>,
    block_merges: Option<bool>,
    forbid_p2_root_rebases: Option<bool>,
    casefolding_check: Option<bool>,
    emit_obsmarkers: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLfsParams {
    threshold: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBundle2ReplayParams {
    preserve_raw_bundle2: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawShardedFilenodesParams {
    shard_map: String,
    shard_num: usize,
}

#[cfg(test)]
mod test {
    use super::*;
    use maplit::{btreemap, hashmap};
    use std::fs::{create_dir_all, write};
    use tempdir::TempDir;

    fn write_files(
        files: impl IntoIterator<Item = (impl AsRef<Path>, impl AsRef<[u8]>)>,
    ) -> TempDir {
        let tmp_dir = TempDir::new("mononoke_test_config").expect("tmp_dir failed");

        for (path, content) in files.into_iter() {
            let path = path.as_ref();
            let content = content.as_ref();

            let dir = path.parent().expect("missing parent");
            create_dir_all(tmp_dir.path().join(dir)).expect("create dir failed");
            write(tmp_dir.path().join(path), content).expect("write failed");
        }

        tmp_dir
    }

    #[test]
    fn test_read_manifest() {
        let hook1_content = "this is hook1";
        let hook2_content = "this is hook2";
        let fbsource_content = r#"
            db_address="db_address"
            write_lock_db_address="write_lock_db_address"
            repotype="blob:remote"
            generation_cache_size=1048576
            repoid=0
            scuba_table="scuba_table"
            blobstore_scuba_table="blobstore_scuba_table"
            skiplist_index_blobstore_key="skiplist_key"
            bookmarks_cache_ttl=5000
            [cache_warmup]
            bookmark="master"
            commit_limit=100
            [hook_manager_params]
            entrylimit=1234
            weightlimit=4321
            disable_acl_checker=false
            [[remote_blobstore]]
            blobstore_id=0
            blobstore_type="manifold"
            manifold_bucket="bucket"
            [[remote_blobstore]]
            blobstore_id=1
            blobstore_type="gluster"
            gluster_tier="mononoke.gluster.tier"
            gluster_export="groot"
            gluster_basepath="mononoke/glusterblob-test"
            [[bookmarks]]
            name="master"
            allowed_users="^(svcscm|twsvcscm)$"
            [[bookmarks.hooks]]
            hook_name="hook1"
            [[bookmarks.hooks]]
            hook_name="hook2"
            [[bookmarks.hooks]]
            hook_name="rust:rusthook"
            [[bookmarks]]
            regex="[^/]*/stable"
            [[hooks]]
            name="hook1"
            path="common/hooks/hook1.lua"
            hook_type="PerAddedOrModifiedFile"
            bypass_commit_string="@allow_hook1"
            [[hooks]]
            name="hook2"
            path="./hooks/hook2.lua"
            hook_type="PerChangeset"
            bypass_pushvar="pushvar=pushval"
            config_strings={ conf1 = "val1", conf2 = "val2" }
            [[hooks]]
            name="rust:rusthook"
            hook_type="PerChangeset"
            config_ints={ int1 = 44 }
            [pushrebase]
            rewritedates = false
            recursion_limit = 1024
            forbid_p2_root_rebases = false
            casefolding_check = false
            emit_obsmarkers = false
            [lfs]
            threshold = 1000
            [bundle2_replay_params]
            preserve_raw_bundle2 = true
            [sharded_filenodes]
            shard_map = "db_address_shards"
            shard_num = 123
        "#;
        let www_content = r#"
            path="/tmp/www"
            repotype="blob:files"
            repoid=1
            scuba_table="scuba_table"
            blobstore_scuba_table="blobstore_scuba_table"
            wireproto_scribe_category="category"
        "#;
        let common_content = r#"
            [[whitelist_entry]]
            tier = "tier1"

            [[whitelist_entry]]
            identity_type = "username"
            identity_data = "user"
        "#;

        let paths = btreemap! {
            "common/common.toml" => common_content,
            "common/hooks/hook1.lua" => hook1_content,
            "repos/fbsource/server.toml" => fbsource_content,
            "repos/fbsource/hooks/hook2.lua" => hook2_content,
            "repos/www/server.toml" => www_content,
            "my_path/my_files" => "",
        };

        let tmp_dir = write_files(&paths);

        let repoconfig = RepoConfigs::read_configs(tmp_dir.path()).expect("failed to read configs");

        let first_manifold_args = ManifoldArgs {
            bucket: "bucket".into(),
            prefix: "".into(),
        };
        let second_gluster_args = GlusterArgs {
            tier: "mononoke.gluster.tier".into(),
            export: "groot".into(),
            basepath: "mononoke/glusterblob-test".into(),
        };
        let mut blobstores = HashMap::new();
        blobstores.insert(
            BlobstoreId::new(0),
            RemoteBlobstoreArgs::Manifold(first_manifold_args),
        );
        blobstores.insert(
            BlobstoreId::new(1),
            RemoteBlobstoreArgs::Gluster(second_gluster_args),
        );
        let blobstores_args = RemoteBlobstoreArgs::Multiplexed {
            scuba_table: Some("blobstore_scuba_table".to_string()),
            blobstores,
        };

        let mut repos = HashMap::new();
        repos.insert(
            "fbsource".to_string(),
            RepoConfig {
                enabled: true,
                repotype: RepoType::BlobRemote {
                    db_address: "db_address".into(),
                    blobstores_args,
                    sharded_filenodes: Some(ShardedFilenodesParams {
                        shard_map: "db_address_shards".into(),
                        shard_num: NonZeroUsize::new(123).unwrap(),
                    }),
                    write_lock_db_address: Some("write_lock_db_address".into()),
                },
                generation_cache_size: 1024 * 1024,
                repoid: 0,
                scuba_table: Some("scuba_table".to_string()),
                cache_warmup: Some(CacheWarmupParams {
                    bookmark: Bookmark::new("master").unwrap(),
                    commit_limit: 100,
                }),
                hook_manager_params: Some(HookManagerParams {
                    entrylimit: 1234,
                    weightlimit: 4321,
                    disable_acl_checker: false,
                }),
                bookmarks_cache_ttl: Some(Duration::from_millis(5000)),
                bookmarks: vec![
                    BookmarkParams {
                        bookmark: Bookmark::new("master").unwrap().into(),
                        hooks: vec![
                            "hook1".to_string(),
                            "hook2".to_string(),
                            "rust:rusthook".to_string(),
                        ],
                        only_fast_forward: false,
                        allowed_users: Some(Regex::new("^(svcscm|twsvcscm)$").unwrap()),
                    },
                    BookmarkParams {
                        bookmark: Regex::new("[^/]*/stable").unwrap().into(),
                        hooks: vec![],
                        only_fast_forward: false,
                        allowed_users: None,
                    },
                ],
                hooks: vec![
                    HookParams {
                        name: "hook1".to_string(),
                        code: Some("this is hook1".to_string()),
                        hook_type: HookType::PerAddedOrModifiedFile,
                        config: HookConfig {
                            bypass: Some(HookBypass::CommitMessage("@allow_hook1".into())),
                            strings: hashmap! {},
                            ints: hashmap! {},
                        },
                    },
                    HookParams {
                        name: "hook2".to_string(),
                        code: Some("this is hook2".to_string()),
                        hook_type: HookType::PerChangeset,
                        config: HookConfig {
                            bypass: Some(HookBypass::Pushvar {
                                name: "pushvar".into(),
                                value: "pushval".into(),
                            }),
                            strings: hashmap! {
                                "conf1".into() => "val1".into(),
                                "conf2".into() => "val2".into(),
                            },
                            ints: hashmap! {},
                        },
                    },
                    HookParams {
                        name: "rust:rusthook".to_string(),
                        code: None,
                        hook_type: HookType::PerChangeset,
                        config: HookConfig {
                            bypass: None,
                            strings: hashmap! {},
                            ints: hashmap! {
                                "int1".into() => 44,
                            },
                        },
                    },
                ],
                pushrebase: PushrebaseParams {
                    rewritedates: false,
                    recursion_limit: 1024,
                    commit_scribe_category: None,
                    block_merges: false,
                    forbid_p2_root_rebases: false,
                    casefolding_check: false,
                    emit_obsmarkers: false,
                },
                lfs: LfsParams {
                    threshold: Some(1000),
                },
                wireproto_scribe_category: None,
                hash_validation_percentage: 0,
                readonly: RepoReadOnly::ReadWrite,
                skiplist_index_blobstore_key: Some("skiplist_key".into()),
                bundle2_replay_params: Bundle2ReplayParams {
                    preserve_raw_bundle2: true,
                },
            },
        );
        repos.insert(
            "www".to_string(),
            RepoConfig {
                enabled: true,
                repotype: RepoType::BlobFiles("/tmp/www".into()),
                generation_cache_size: 10 * 1024 * 1024,
                repoid: 1,
                scuba_table: Some("scuba_table".to_string()),
                cache_warmup: None,
                hook_manager_params: None,
                bookmarks: vec![],
                bookmarks_cache_ttl: None,
                hooks: vec![],
                pushrebase: Default::default(),
                lfs: Default::default(),
                wireproto_scribe_category: Some("category".to_string()),
                hash_validation_percentage: 0,
                readonly: RepoReadOnly::ReadWrite,
                skiplist_index_blobstore_key: None,
                bundle2_replay_params: Bundle2ReplayParams::default(),
            },
        );
        assert_eq!(
            repoconfig,
            RepoConfigs {
                metaconfig: MetaConfig {},
                repos,
                common: CommonConfig {
                    security_config: vec![
                        WhitelistEntry::Tier("tier1".to_string()),
                        WhitelistEntry::HardcodedIdentity {
                            ty: "username".to_string(),
                            data: "user".to_string(),
                        },
                    ],
                },
            }
        )
    }

    #[test]
    fn test_broken_config() {
        // Two bypasses for one hook
        let hook1_content = "this is hook1";
        let content = r#"
            path="/tmp/fbsource"
            repotype="blob:rocks"
            repoid=0
            [[bookmarks]]
            name="master"
            [[bookmarks.hooks]]
            hook_name="hook1"
            [[hooks]]
            name="hook1"
            path="common/hooks/hook1.lua"
            hook_type="PerAddedOrModifiedFile"
            bypass_commit_string="@allow_hook1"
            bypass_pushvar="var=val"
        "#;

        let paths = btreemap! {
            "common/hooks/hook1.lua" => hook1_content,
            "repos/fbsource/server.toml" => content,
        };

        let tmp_dir = write_files(&paths);

        let res = RepoConfigs::read_configs(tmp_dir.path());
        assert!(res.is_err());

        // Incorrect bypass string
        let hook1_content = "this is hook1";
        let content = r#"
            path="/tmp/fbsource"
            repotype="blob:rocks"
            repoid=0
            [[bookmarks]]
            name="master"
            [[bookmarks.hooks]]
            hook_name="hook1"
            [[hooks]]
            name="hook1"
            path="common/hooks/hook1.lua"
            hook_type="PerAddedOrModifiedFile"
            bypass_pushvar="var"
        "#;

        let paths = btreemap! {
            "common/hooks/hook1.lua" => hook1_content,
            "repos/fbsource/server.toml" => content,
        };

        let tmp_dir = write_files(&paths);

        let res = RepoConfigs::read_configs(tmp_dir.path());
        assert!(res.is_err());
    }

    #[test]
    fn test_broken_common_config() {
        fn check_fails(common: &str) -> bool {
            let content = r#"
                path="/tmp/fbsource"
                repotype="blob:rocks"
                repoid=0
            "#;

            let paths = btreemap! {
                "common/common.toml" => common,
                "repos/fbsource/server.toml" => content,
            };

            let tmp_dir = write_files(&paths);

            let res = RepoConfigs::read_configs(tmp_dir.path());
            res.is_err()
        }

        let common = r#"
        [[whitelist_entry]]
        identity_type="user"
        "#;
        assert!(check_fails(common));

        let common = r#"
        [[whitelist_entry]]
        identity_data="user"
        "#;
        assert!(check_fails(common));

        let common = r#"
        [[whitelist_entry]]
        tier="user"
        identity_type="user"
        identity_data="user"
        "#;
        assert!(check_fails(common));

        // Only one tier is allowed
        let common = r#"
        [[whitelist_entry]]
        tier="tier1"
        [[whitelist_entry]]
        tier="tier2"
        "#;
        assert!(check_fails(common));
    }
}
