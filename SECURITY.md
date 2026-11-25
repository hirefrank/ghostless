# Security

ghostless keeps the corpus, drafts, transcripts, and voice stats on the machine where it runs. The CLI does not upload that writing.

The optional update check is a GET of the public GitHub releases endpoint. It sends no corpus, draft, or transcript. Turn it off with `GHOSTLESS_NO_UPDATE=1`, `GHOSTLESS_UPDATE_CHECK=0`, `"updateCheck": false` in `~/.ghostless/config.json`, or the same flag in the project manifest.

## Reporting

Report a vulnerability privately through [GitHub Security Advisories](https://github.com/hirefrank/ghostless/security/advisories/new). Please do not open a public issue for an unfixed vulnerability.
