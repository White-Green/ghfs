use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use base64::DecodeError;
use regex::Regex;
use serde_derive::Deserialize;

use crate::github_filesystem::http::HTTPWrapper;

#[derive(Debug, Deserialize)]
pub(crate) struct GitHubApiRoot {
    owner: String,
    repo: String,
    branch_name: Option<String>,
    rev: Option<String>,
}

#[derive(Debug)]
pub(crate) enum GitHubApiRootConstructError {
    InvalidUrl(String),
    InvalidRev(String),
}

impl Display for GitHubApiRootConstructError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            GitHubApiRootConstructError::InvalidUrl(url) => write!(f, "URL {:?} is invalid.", url),
            GitHubApiRootConstructError::InvalidRev(rev) => write!(f, "rev {:?} is invalid.", rev),
        }
    }
}

impl Error for GitHubApiRootConstructError {}

impl GitHubApiRoot {
    pub(crate) fn from_repository_url(url: &str) -> Result<Self, GitHubApiRootConstructError> {
        let regex = Regex::new("^(https://github\\.com/)?(?P<owner>[^/]+)/(?P<repo>[^/?]+)(\\?(branch=(?P<branch>.+)|rev=(?P<rev>.+))?)?$").unwrap();
        let captures = regex.captures(url).ok_or_else(|| GitHubApiRootConstructError::InvalidUrl(url.to_string()))?;
        let result = GitHubApiRoot {
            owner: captures.name("owner").unwrap().as_str().to_string(),
            repo: captures.name("repo").unwrap().as_str().to_string(),
            branch_name: captures.name("branch").map(|s| s.as_str().to_string()),
            rev: captures.name("rev").map(|s| s.as_str().to_string()),
        };
        assert!(result.branch_name.is_none() || result.rev.is_none());
        if let Some(rev) = &result.rev {
            let rev_regex = Regex::new("^[0-9a-f]{40}$").unwrap();
            if !rev_regex.is_match(&rev) {
                return Err(GitHubApiRootConstructError::InvalidRev(rev.clone()));
            }
        }
        Ok(result)
    }

    pub(crate) async fn get_repository_tree_url(&self, http: &HTTPWrapper) -> Result<Tree, reqwest::Error> {
        match self {
            GitHubApiRoot { owner, repo, branch_name: Option::None, rev: Option::None } => {
                let repository = http.request::<Repository>(&format!("https://api.github.com/repos/{}/{}", owner, repo)).await?;
                let branch = http.request::<Branch>(&format!("https://api.github.com/repos/{}/{}/branches/{}", owner, repo, repository.default_branch)).await?;
                Ok(Tree { path: "".to_string(), url: branch.into_tree_url() })
            }
            GitHubApiRoot { owner, repo, branch_name: Some(branch_name), rev: Option::None } => {
                let branch = http.request::<Branch>(&format!("https://api.github.com/repos/{}/{}/branches/{}", owner, repo, branch_name)).await?;
                Ok(Tree { path: "".to_string(), url: branch.into_tree_url() })
            }
            GitHubApiRoot { owner, repo, branch_name: Option::None, rev: Some(rev) } => {
                let commit = http.request::<BranchCommit>(&format!("https://api.github.com/repos/{}/{}/commits/{}", owner, repo, rev)).await?;
                Ok(Tree { path: "".to_string(), url: commit.commit.tree.url })
            }
            GitHubApiRoot { branch_name: Some(_), rev: Some(_), .. } => unreachable!(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct Repository {
    default_branch: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Branch {
    commit: BranchCommit,
}

impl Branch {
    fn into_tree_url(self) -> String {
        self.commit.commit.tree.url
    }
}

#[derive(Debug, Deserialize)]
struct BranchCommit {
    commit: Commit,
}

#[derive(Debug, Deserialize)]
struct Commit {
    tree: BranchCommitCommitTree,
}

#[derive(Debug, Deserialize)]
struct BranchCommitCommitTree {
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum TreeItem {
    #[serde(rename = "blob")]
    Blob(Blob),
    #[serde(rename = "tree")]
    Tree(Tree),
}

#[derive(Debug, Deserialize)]
pub(crate) struct Blob {
    path: String,
    size: usize,
    url: String,
}

#[derive(Debug, Deserialize)]
struct BlobContent {
    content: String,
    encoding: String,
}

#[derive(Debug)]
pub(crate) enum GetBlobContentError {
    HTTPError(reqwest::Error),
    DecodeError(base64::DecodeError),
}

impl From<reqwest::Error> for GetBlobContentError {
    fn from(e: reqwest::Error) -> Self {
        GetBlobContentError::HTTPError(e)
    }
}

impl From<base64::DecodeError> for GetBlobContentError {
    fn from(e: DecodeError) -> Self {
        GetBlobContentError::DecodeError(e)
    }
}

impl Display for GetBlobContentError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            GetBlobContentError::HTTPError(e) => Display::fmt(e, f),
            GetBlobContentError::DecodeError(e) => Display::fmt(e, f),
        }
    }
}

impl Error for GetBlobContentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            GetBlobContentError::HTTPError(e) => Some(e),
            GetBlobContentError::DecodeError(e) => Some(e),
        }
    }
}

impl Blob {
    pub(crate) fn path(&self) -> &str {
        &self.path
    }
    pub(crate) fn size(&self) -> usize {
        self.size
    }
    pub(crate) async fn into_content(self, http: Arc<HTTPWrapper>) -> Result<Vec<u8>, GetBlobContentError> {
        let blob = http.request::<BlobContent>(&self.url).await?;
        assert_eq!(&blob.encoding, "base64", "blob encoding {:?} is not supported", &blob.encoding);
        base64::decode(&blob.content.replace("\n", "")).map_err(Into::into)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct Tree {
    path: String,
    url: String,
}

impl Tree {
    pub(crate) fn path(&self) -> &str {
        &self.path
    }
    pub(crate) async fn into_tree_items(self, http: Arc<HTTPWrapper>) -> Result<Vec<TreeItem>, reqwest::Error> {
        http.request::<TreeResponse>(&self.url).await.map(|tree| tree.tree)
    }
}

#[derive(Debug, Deserialize)]
struct TreeResponse {
    tree: Vec<TreeItem>,
}
