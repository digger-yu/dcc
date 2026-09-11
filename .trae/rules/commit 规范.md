commit提交要求:
   - commit message subject 简洁
   - body 用英文, 详细说明每个模块的修改内容
   - 正文需要解释根因和修复理由
   - body 每行 ≤ 72 字符
   - 不加 Co-authored-by 除非用户明确允许
   - 用 `git commit -s -S` (SSH 签名, 因为 gpg.format=ssh)
   - Signed-off-by 只能有一个: `digger yu <digger-yu@outlook.com>`
   - 仅 commit 到本地仓库, 不 push