# ghfs
GitHubリポジトリをファイルにマウントしてアクセスできるよ
APIで非同期にアクセスするから、でかいリポジトリをcloneしたくないときに使えるよ

# Require
- OS:Linux(macでも動くかも(Dokanyにも対応したいね))
- FUSE

# Usage
```
$ cargo run -- https://github.com/<owner>/<repo> /path/to/directory
```
You can see help for parameters with `--help`.