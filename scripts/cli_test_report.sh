#!/usr/bin/env bash
# oray-tools CLI 测试套件：逐条执行测试指令，把指令、stdout/stderr、退出码
# 全部写进一份 markdown 报告。写操作在测后恢复原始设备状态。
#
#   ./scripts/cli_test_report.sh [output.md]   (default: TEST_REPORT.md)
#
# 注意：
#  * 登录（auth login / auth login-sms）由使用者自行验证，本套件不重复。
#  * 设备上用户自建的定时器（01:00 每天 关闭）是只读基线，绝不删除。
#  * 所有会改配置的用例都作用在副本上，真实 ~/.config 不会被改写。
set -u

OUT="${1:-TEST_REPORT.md}"
# 被测二进制：默认用系统安装的 oray-tools；验证本地补丁时传
#   ORAY=./target/debug/oray-tools ./scripts/cli_test_report.sh
ORAY="${ORAY:-oray-tools}"
WORK="$(mktemp -d ./tmp/testrun.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

SN="560056660997"          # wakeup 设备
ID="1710192461"            # remote 设备
NAME0="智能插座C1Pro-BLE-V3 (蓝牙版)"
RNAME0="DESKTOP-BOTVVJ6"
RMEMO0="这个是备注"
REAL_CFG="$HOME/.config/oray-tools/config.toml"
MY_TIMER_TIME="23:55"      # 本套件自建的定时器时间（与用户自建的 01:00 区分）

: > "$OUT"
fail=0

sec()   { printf '\n## %s\n' "$1" >> "$OUT"; }
note()  { printf '%s\n' "$1" >> "$OUT"; }
flag()  { fail=$((fail+1)); printf '> ⚠️ %s\n' "$1" >> "$OUT"; }

# t "<说明>" "<shell 命令>"   —— 超时用 $TMO（默认 90s）
t() {
  local desc="$1" cmd="$2" so se rc
  so="$WORK/out"; se="$WORK/err"
  timeout "${TMO:-90}" bash -o pipefail -c "$cmd" >"$so" 2>"$se"
  rc=$?
  {
    printf '### %s\n\n' "$desc"
    printf '```console\n$ %s\n' "$cmd"
    if [ -s "$so" ]; then printf '\n'; cat "$so"; fi
    if [ -s "$se" ]; then printf '\n[stderr]\n'; cat "$se"; fi
    printf '[exit code: %d]\n```\n' "$rc"
  } >> "$OUT"
  return $rc
}

# 与 t 相同，但 stdin 关闭（验证非 TTY 行为，不会阻塞）
t_notty() {
  local desc="$1" cmd="$2" so se rc
  so="$WORK/out"; se="$WORK/err"
  timeout "${TMO:-30}" bash -o pipefail -c "$cmd" >"$so" 2>"$se" </dev/null
  rc=$?
  {
    printf '### %s\n\n' "$desc"
    printf '```console\n$ %s   # stdin </dev/null\n' "$cmd"
    if [ -s "$so" ]; then printf '\n'; cat "$so"; fi
    if [ -s "$se" ]; then printf '\n[stderr]\n'; cat "$se"; fi
    printf '[exit code: %d]\n```\n' "$rc"
  } >> "$OUT"
  return $rc
}

expect_zero()    { [ "$1" -ne 0 ] && flag "预期 exit 0，实际 $1（见上）" || true; }
expect_nonzero() { [ "$1" -eq 0 ] && flag "预期非 0 退出码，实际 0（见上）" || true; }

# 某配置文件里 access_token 的长度（用于识别“占位符令牌”缺陷）
access_len() { python3 -c "import tomllib,sys;print(len(tomllib.load(open(sys.argv[1],'rb'))['token']['access_token']))" "$1" 2>/dev/null || echo 0; }

{
  printf '# oray-tools CLI 测试报告\n\n'
  printf -- '- 时间: %s\n' "$(date '+%Y-%m-%d %H:%M:%S %z')"
  printf -- '- 版本: %s\n' "$($ORAY --version 2>&1)"
  printf -- '- 被测二进制: %s%s\n' "$(command -v "$ORAY" 2>/dev/null || echo "$ORAY")" "$([ "$ORAY" = oray-tools ] && echo '（系统安装版）' || echo '（本地构建，含保存前校验补丁）')"
  printf -- '- 主机: %s / %s\n' "$(uname -s)" "$(uname -m)"
  printf -- '- 测试对象: wakeup 设备 **sn=%s**（1 路智能插座，基线 ON）、remote 设备 **id=%s**（离线 Windows 主机）\n' "$SN" "$ID"
  printf -- '- 登录相关（auth login / auth login-sms）由使用者自行验证，本报告不重复\n'
  printf -- '- 写操作测后均恢复原状，证据见末尾「状态恢复核对」\n'
  printf -- '- 设备上用户自建的定时器（01:00 每天 关闭）为只读基线，套件不删除它\n'
} >> "$OUT"

# ---------------------------------------------------------------- A. CLI 基础
sec "A. CLI 基础与帮助"
t "版本号" "$ORAY --version"; expect_zero $?
t "根帮助" "$ORAY --help"; expect_zero $?
t "无参数调用（clap 用法错误）" "oray-tools"; expect_nonzero $?
t "wakeup 组帮助" "$ORAY wakeup --help"; expect_zero $?
t "remote 组帮助" "$ORAY remote --help"; expect_zero $?
t "wakeup plug 组帮助" "$ORAY wakeup plug --help"; expect_zero $?
t "help 链路 help wakeup plug timer" "$ORAY help wakeup plug timer"; expect_zero $?
t "缺参数 auth login（不联网，clap 原生错误）" "$ORAY auth login"; expect_nonzero $?
t "非法位置参数 remote info abc" "$ORAY remote info abc"; expect_nonzero $?
t "缺必填项 wakeup plug timer add（无 --time）" "$ORAY wakeup plug timer add $SN"; expect_nonzero $?
t "缺必填项 wakeup plug countdown start（无 --count）" "$ORAY wakeup plug countdown start"; expect_nonzero $?

# ------------------------------------------------- B. 错误处理 / --json 契约
sec "B. 错误处理与 --json 契约"
t "wakeup info 不存在的 SN（文本模式，错误在 stderr）" "$ORAY wakeup info 999999999999"; expect_nonzero $?
t "wakeup info 不存在的 SN（--json：stdout 单个 {ok:false}）" "$ORAY --json wakeup info 999999999999"; expect_nonzero $?
t "remote info 不存在的 id（文本模式）" "$ORAY remote info 999999999"; expect_nonzero $?
t "remote info 不存在的 id（--json）" "$ORAY --json remote info 999999999"; expect_nonzero $?
t "非法 --tz" "$ORAY --tz bogus wakeup list"; expect_nonzero $?
t "非法 --tz（--json）" "$ORAY --json --tz bogus wakeup list"; expect_nonzero $?
t "非法 --time 25:00" "$ORAY wakeup plug timer add $SN --time 25:00"; expect_nonzero $?
t "非法时间窗口 --since 3天" "$ORAY wakeup plug logs $SN --since 3天"; expect_nonzero $?
t "非法 --index（非数字）" "$ORAY wakeup plug status $SN --index x"; expect_nonzero $?
t "不存在的 --config 路径（错误需指名路径）" "$ORAY --config $WORK/no-such.toml auth status"; expect_nonzero $?
t "wakeup plug led 非法状态值" "$ORAY wakeup plug led $SN maybe"; expect_nonzero $?
t "wakeup plug power-on-restore 非法状态值 1" "$ORAY wakeup plug power-on-restore $SN 1"; expect_nonzero $?

# ------------------------------------------------------------- C. auth（非登录）
sec "C. auth（除 login / login-sms 外）"
t "auth status（令牌默认掩码）" "$ORAY auth status"; expect_zero $?
t "auth status --show（完整令牌）" "$ORAY auth status --show"; expect_zero $?
t "auth status --json" "$ORAY --json auth status"; expect_zero $?
t "auth refresh（真实配置；本会话 ~/.config 只读 → 预期写入失败）" "$ORAY auth refresh"; expect_nonzero $?
note "> 这条 exit 1 有两个可能来源，二者都不会改写配置：本会话 \`~/.config\` 只读（EROFS），"
note "> 或服务端返回占位符令牌被「保存前校验」拒绝（见 H 节）。网络侧 refresh 是否成功由 H 节"
note "> 可写副本上的用例覆盖。"
t "auth status（真实登录态未被影响）" "$ORAY auth status"; expect_zero $?

cp "$REAL_CFG" "$WORK/cfg_logout.toml"
t "auth logout（配置副本，不影响真实登录态）" "$ORAY --config $WORK/cfg_logout.toml auth logout"; expect_zero $?
t "logout 后 auth status（打印 no tokens saved，按设计 exit 0）" "$ORAY --config $WORK/cfg_logout.toml auth status"; expect_zero $?
t "logout 后 wakeup list（应报未登录，exit 1）" "$ORAY --config $WORK/cfg_logout.toml wakeup list"; expect_nonzero $?
t "logout 后 auth status --json" "$ORAY --json --config $WORK/cfg_logout.toml auth status"; expect_zero $?
t "真实配置未被上述副本用例影响" "$ORAY auth status"; expect_zero $?

# --------------------------------------------------------- D. wakeup 只读
sec "D. wakeup 只读命令"
t "wakeup list" "$ORAY wakeup list"; expect_zero $?
t "wakeup list --json" "$ORAY --json wakeup list"; expect_zero $?
t "wakeup list --offset 1（翻到空页）" "$ORAY wakeup list --offset 1"; expect_zero $?
t "wakeup list --limit 1" "$ORAY wakeup list --limit 1"; expect_zero $?
t "wakeup info <SN>" "$ORAY wakeup info $SN"; expect_zero $?
t "wakeup info <SN> --json" "$ORAY --json wakeup info $SN"; expect_zero $?
t "plug status" "$ORAY wakeup plug status $SN"; expect_zero $?
t "plug status --json" "$ORAY --json wakeup plug status $SN"; expect_zero $?
t "plug status --index 1（1 路插座越界端口，服务端回退 index=0）" "$ORAY wakeup plug status $SN --index 1"; expect_zero $?
t "plug logs" "$ORAY wakeup plug logs $SN"; expect_zero $?
t "plug logs --index 0" "$ORAY wakeup plug logs $SN --index 0"; expect_zero $?
t "plug logs --since 1d（时区告警）" "$ORAY wakeup plug logs $SN --since 1d"; expect_zero $?
t "plug logs --page 1" "$ORAY wakeup plug logs $SN --page 1"; expect_zero $?
t "plug logs --json --since 2d" "$ORAY --json wakeup plug logs $SN --since 2d"; expect_zero $?
t "plug timer list（基线：仅用户自建的 01:00 定时器）" "$ORAY wakeup plug timer list $SN"; expect_zero $?
t "plug countdown status" "$ORAY wakeup plug countdown status $SN"; expect_zero $?

# ------------------------------------------------------- E. wakeup 写操作
sec "E. wakeup 写操作（测后恢复；用户自建定时器不动）"
t "【基线】plug status" "$ORAY wakeup plug status $SN"; expect_zero $?
t "plug off" "$ORAY wakeup plug off $SN"; expect_zero $?
t "plug status（应为 OFF）" "$ORAY wakeup plug status $SN"; expect_zero $?
t "plug on" "$ORAY wakeup plug on $SN"; expect_zero $?
t "plug status（恢复 ON）" "$ORAY wakeup plug status $SN"; expect_zero $?
t "plug led off" "$ORAY wakeup plug led $SN off"; expect_zero $?
t "plug status --json（led 应为 0）" "$ORAY --json wakeup plug status $SN"; expect_zero $?
t "plug led on" "$ORAY wakeup plug led $SN on"; expect_zero $?
t "plug status --json（led 恢复 1）" "$ORAY --json wakeup plug status $SN"; expect_zero $?
t "power-on-restore 0" "$ORAY wakeup plug power-on-restore $SN 0"; expect_zero $?
t "plug status --json（def_st 应为 0）" "$ORAY --json wakeup plug status $SN"; expect_zero $?
t "power-on-restore 2（恢复原值）" "$ORAY wakeup plug power-on-restore $SN 2"; expect_zero $?
t "plug status --json（def_st 恢复 2）" "$ORAY --json wakeup plug status $SN"; expect_zero $?

t "timer add（本套件自建 ${MY_TIMER_TIME} 单次开）" "$ORAY wakeup plug timer add $SN --time $MY_TIMER_TIME --action 1"; expect_zero $?
t "timer list（应出现新定时器，且保留用户 01:00 定时器）" "$ORAY wakeup plug timer list $SN"; expect_zero $?
tid=$($ORAY wakeup plug timer list $SN --json 2>/dev/null | tr -d ' \n' | grep -o '"timer_id":[0-9]*' | head -1 | cut -d: -f2)
note ""
note "> 取到的本套件 timer id: \`${tid:-<未取到>}\`"
if [ -n "${tid:-}" ]; then
  t "timer disable $tid" "$ORAY wakeup plug timer disable $SN $tid"; expect_zero $?
  t "timer list（该定时器应标记 disabled，用户定时器不受影响）" "$ORAY wakeup plug timer list $SN"; expect_zero $?
  t "timer enable $tid" "$ORAY wakeup plug timer enable $SN $tid"; expect_zero $?
  t "timer list（应重新启用）" "$ORAY wakeup plug timer list $SN"; expect_zero $?
  t "timer remove $tid（只删本套件建的）" "$ORAY wakeup plug timer remove $SN $tid"; expect_zero $?
  t "timer remove 不存在的 id（应报错）" "$ORAY wakeup plug timer remove $SN 9999999999"; expect_nonzero $?
else
  flag "未能取得 timer id，timer disable/enable/remove 未执行"
fi
t "timer list（本套件定时器已清除，仅剩用户 01:00 定时器）" "$ORAY wakeup plug timer list $SN"; expect_zero $?

t "countdown start（900 秒后关闭）" "$ORAY wakeup plug countdown start $SN --count 900 --action 0"; expect_zero $?
t "countdown status（应显示运行中）" "$ORAY wakeup plug countdown status $SN"; expect_zero $?
t "countdown stop（清理，防止真实断电）" "$ORAY wakeup plug countdown stop $SN"; expect_zero $?
t "countdown status（应无倒计时）" "$ORAY wakeup plug countdown status $SN"; expect_zero $?

t "wakeup rename → 测试改名" "$ORAY wakeup rename $SN 测试改名"; expect_zero $?
t "wakeup info（显示新名称）" "$ORAY wakeup info $SN"; expect_zero $?
t "wakeup rename → 恢复原名" "$ORAY wakeup rename $SN '$NAME0'"; expect_zero $?
t "wakeup info（名称已恢复）" "$ORAY wakeup info $SN"; expect_zero $?
t "wakeup memo → 设置备注" "$ORAY wakeup memo $SN 测试备注"; expect_zero $?
t "wakeup info（应显示 memo）" "$ORAY wakeup info $SN"; expect_zero $?
t "wakeup memo → 清空恢复" "$ORAY wakeup memo $SN ''"; expect_zero $?
t "wakeup info（memo 已消失，恢复为空）" "$ORAY wakeup info $SN"; expect_zero $?

# -------------------------------------------------------------- F. remote 写操作
sec "F. remote 写操作（测后恢复）"
t "【基线】remote info" "$ORAY remote info $ID"; expect_zero $?
t "remote rename → 测试改名" "$ORAY remote rename $ID 测试改名"; expect_zero $?
t "remote info（显示新名称）" "$ORAY remote info $ID"; expect_zero $?
t "remote rename → 恢复原名" "$ORAY remote rename $ID $RNAME0"; expect_zero $?
t "remote info（名称已恢复）" "$ORAY remote info $ID"; expect_zero $?
t "remote memo → 改备注" "$ORAY remote memo $ID 测试备注remote"; expect_zero $?
t "remote info（显示新备注）" "$ORAY remote info $ID"; expect_zero $?
t "remote memo → 恢复原备注" "$ORAY remote memo $ID '$RMEMO0'"; expect_zero $?
t "remote info（备注已恢复）" "$ORAY remote info $ID"; expect_zero $?
t "remote status" "$ORAY remote status $ID"; expect_zero $?
t "remote status --json" "$ORAY --json remote status $ID"; expect_zero $?
t "remote list --limit 1" "$ORAY remote list --limit 1"; expect_zero $?

# --------------------------------------------------------- G. 全局参数
sec "G. 全局参数"
cp "$REAL_CFG" "$WORK/cfg_g.toml"
t "--clientid 覆盖（写入配置副本，不污染真实配置）" "$ORAY --config $WORK/cfg_g.toml --clientid test-clientid-0001 wakeup list"; expect_zero $?
t "副本中的 clientid 已持久化" "grep -o 'clientid = .*' $WORK/cfg_g.toml"; expect_zero $?
t "--verbose（默认脱敏：令牌应显示为 ***）" "$ORAY --verbose wakeup list 2>&1 | sed -E 's/(eyJ[A-Za-z0-9_-]{8})[A-Za-z0-9_.-]+/\\1…[REDACTED]/g'"; expect_zero $?
t "--verbose --trace-raw（原始跟踪，报告中令牌已脱敏）" "$ORAY --verbose --trace-raw wakeup info $SN 2>&1 | sed -E 's/(eyJ[A-Za-z0-9_-]{8})[A-Za-z0-9_.-]+/\\1…[REDACTED]/g'"; expect_zero $?
t "--tz +9h（显式时区，时间戳应偏移）" "$ORAY --tz +9h wakeup plug logs $SN --since 1d"; expect_zero $?
t "无符号 --tz 480（按设计必须带符号 → 应报错）" "$ORAY --tz 480 wakeup plug logs $SN --since 1d"; expect_nonzero $?
t "+480min（分钟形式，合法）" "$ORAY --tz +480min wakeup plug logs $SN --since 1d"; expect_zero $?
t_notty "--interactive 在非 TTY 下（报需要终端，不阻塞）" "$ORAY wakeup info --interactive"

# --------------------------------------- H. 令牌过期与刷新（工作区过期令牌的副本）
sec "H. 令牌过期与刷新（使用工作区的过期测试令牌，全部作用于副本）"
note "> 素材（均为副本，原件不动）：\`1.toml\` access 过期 441h + refresh 有效；"
note "> \`config.toml\` access 与 refresh 都过期；\`2.toml\` access 过期 + refresh 有效；\`3.toml\` 两者都有效。"
note ">"
note "> **本节为记录型**：刷新接口对同一请求可能返回三种形态 —— 真实令牌 / HTTP 401 invalid refreshtoken /"
note "> HTTP 200 占位符令牌（access_token=\"a\"），取决于服务端配额与令牌状态。因此这里记录实测结果与"
note "> 刷新后的令牌长度，不逐条做通过与否的断言；判定为缺陷的只有下方带 ⚠️ 的条目。"
cp 1.toml "$WORK/exp_access.toml"
t "access 已过期 + refresh 有效 → 自动刷新（记录实测结果）" "$ORAY --config $WORK/exp_access.toml wakeup list"
note "> 刷新后 access_token 长度: $(access_len "$WORK/exp_access.toml")（真实令牌约 378，占位符为 1）"
t "同一配置第二次调用（记录实测结果）" "$ORAY --config $WORK/exp_access.toml wakeup list"
t "auth refresh（副本，记录实测结果）" "$ORAY --config $WORK/exp_access.toml auth refresh"
t "auth status（副本，记录实测结果）" "$ORAY --config $WORK/exp_access.toml auth status"
t "过期 access + --refresh-on-expired（观察是否触发刷新并重试）" "$ORAY --config $WORK/exp_access.toml wakeup list --refresh-on-expired"
note "> 判读：出现 \`access token expired; refreshing and retrying...\` 即「刷新并重试一次」机制被触发；"
note "> 重试是否成功取决于本次刷新拿到的是真实令牌还是占位符（见下方 ⚠️）。"

cp config.toml "$WORK/both_expired.toml"
t "access 与 refresh 均过期（本地记录已过期 19 天）→ wakeup list（记录）" "$ORAY --config $WORK/both_expired.toml wakeup list"
t "access 与 refresh 均过期 → auth refresh（记录）" "$ORAY --config $WORK/both_expired.toml auth refresh"
note "> 观察：若上两条成功，说明服务端并不按本地记录的 \`refresh_expires\` 拒绝刷新 —— 本地显示已过期"
note "> 19 天的 refresh 令牌仍能用，\`refresh_expires\` 实际只由客户端记录。"

cp 2.toml "$WORK/first_use.toml"
t "refresh 令牌使用（auth refresh，副本 A，记录）" "$ORAY --config $WORK/first_use.toml auth refresh"
note "> 刷新后 access_token 长度: $(access_len "$WORK/first_use.toml")"
cp 2.toml "$WORK/second_use.toml"
t "同一个 refresh 令牌第二次使用（副本 B，服务端应拒绝 invalid refreshtoken 或返回占位符）" "$ORAY --config $WORK/second_use.toml auth refresh"
LEN2=$(access_len "$WORK/second_use.toml")
note "> 第二次使用的结果: access_token 长度 $LEN2（真实令牌约 378，占位符为 1）"
[ "$LEN2" -lt 10 ] && flag "补丁未生效：已消费 refresh 令牌的占位符应答（access_token 长度 $LEN2）被写入了配置"

# 补丁验证：占位符令牌不得写入配置（保存前校验 persist_refreshed）
cp "$REAL_CFG" "$WORK/placeholder.toml"
LEN_BEFORE=$(access_len "$WORK/placeholder.toml")
t "已消费过的 refresh 令牌 → auth refresh（校验应在写盘前拒绝占位符）" "$ORAY --config $WORK/placeholder.toml auth refresh"
LEN_AFTER=$(access_len "$WORK/placeholder.toml")
note "> access_token 长度: $LEN_BEFORE → $LEN_AFTER（真实令牌约 378，占位符为 1）"
if [ "$LEN_AFTER" -lt 10 ]; then
  flag "补丁未生效：占位符令牌仍被写入配置（长度 $LEN_BEFORE → $LEN_AFTER）"
  t "用被污染的配置继续调用（应 401 失败）" "$ORAY --config $WORK/placeholder.toml wakeup list"; expect_nonzero $?
else
  note "> ✅ 校验生效：配置未被占位符污染 —— 服务端回占位符时命令失败并保留原配置，回真令牌时正常保存。"
  t "被拒的刷新之后，配置仍可正常调用" "$ORAY --config $WORK/placeholder.toml wakeup list"; expect_zero $?
fi

note ""
note "> \`--refresh-on-expired\` 的「本地有效但服务端拒绝」重试分支需要服务端主动拒签一个本地未过期的"
note "> 令牌，在不伪造签名的情况下无法构造；该分支由单元测试覆盖（\`status_401_without_authorization_"
note "> is_not_expiry\`、\`token_expired_*\`），端到端只验证到「触发刷新并重试一次」这一行为本身。"

# ------------------------------------------------------- I. 工程质量门禁
sec "I. 工程质量门禁（cargo）"
TMO=900
t "cargo fmt --check" "cargo fmt --check 2>&1"; expect_zero $?
t "cargo clippy --workspace --all-targets -- -D warnings" "cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20"; expect_zero $?
t "cargo test --workspace（汇总 + token 模块全部用例）" "cargo test --workspace 2>&1 | grep -E '^running [0-9]+ tests|^test result:|^test token::tests::'"; expect_zero $?
TMO=90

# ------------------------------------------------------------ J. 恢复核对
sec "J. 状态恢复核对"
t "plug status（应为 ON）" "$ORAY wakeup plug status $SN"; expect_zero $?
t "plug status --json（led=1 / def_st=2）" "$ORAY --json wakeup plug status $SN"; expect_zero $?
t "timer list（本套件定时器已清除，用户 01:00 定时器仍在）" "$ORAY wakeup plug timer list $SN"; expect_zero $?
t "countdown status（应为空）" "$ORAY wakeup plug countdown status $SN"; expect_zero $?
t "wakeup info（名称已恢复）" "$ORAY wakeup info $SN"; expect_zero $?
t "wakeup list --json（description 应恢复为 null）" "$ORAY --json wakeup list"; expect_zero $?
t "remote info（名称/备注已恢复）" "$ORAY remote info $ID"; expect_zero $?
t "auth status（登录态仍有效）" "$ORAY auth status"; expect_zero $?
note "> 真实配置 access_token 长度: $(access_len "$REAL_CFG")（应约 378，非占位符）"

{
  printf '\n## 发现与说明\n\n'
  printf '1. **占位符令牌缺陷 → 已打补丁**：`/authorize/refreshing` 会以 HTTP 200 返回占位符令牌'
  printf '（`access_token="a"`、`refresh_token="r"`），旧版会打印 `tokens refreshed` 并把 1 字符令牌'
  printf '写进配置，随后所有命令 401、只能重新登录。补丁在 `crates/oray-cli/src/token.rs` 新增'
  printf '`validate_refresh_response` + `persist_refreshed`：`auth refresh` 与自动刷新（ensure_token）'
  printf '统一先校验 —— access_token 必须是带数值 `exp` 的三段 JWT、refresh_token ≥ 16 字符，'
  printf '不通过就报错退出并保留原配置，错误信息只报字段长度、不回显令牌。'
  printf '单测 5 个：placeholder / short_refresh_token / three_segment_jwt / missing_exp / '
  printf 'persist_refreshed_refuses_to_overwrite_the_saved_config；H 节的补丁用例断言配置长度不变。\n'
  printf '2. **本会话环境限制**：`~/.config/oray-tools` 被挂成只读（EROFS），因此针对真实配置的 '
  printf '`auth refresh` / `logout` 无法落盘；这类用例一律改在可写副本上执行，真实登录态未被改动。\n'
  printf '3. **服务端刷新接口不稳定**：同一请求会返回三种形态（真实令牌 / 401 invalid refreshtoken / '
  printf '200 占位符），H 节为记录型用例，只留证据不判通过与否。\n'
  printf '4. **`--refresh-on-expired` 的重试分支**：需要服务端拒绝一个本地未过期的令牌，在不伪造签名的'
  printf '前提下无法构造，端到端只验证到「触发刷新并重试一次」，该分支由单元测试覆盖。\n'
  printf '\n## 结论\n\n'
  if [ "$fail" -eq 0 ]; then
    printf '所有断言型检查点与预期一致。\n'
  else
    printf '有 **%d** 处与预期不符 / 缺陷复现，见上文 ⚠️ 标记。\n' "$fail"
  fi
} >> "$OUT"

echo "report: $OUT  (unexpected=$fail)"
exit "$fail"
