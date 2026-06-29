#!/usr/bin/env bash
#
# analyze_hls_ads.sh — 对一个 TVBox/CMS 采集站 API 抽样若干视频，
# 解析到 media 级 m3u8，输出每个视频的“广告指纹”，用于判断该源的插片广告形态。
#
# 用法:
#   scripts/analyze_hls_ads.sh <采集站API地址> [视频数=6]
#
# 示例:
#   scripts/analyze_hls_ads.sh 'https://ikunzy.com/api.php/provide/vod/' 8
#
# 在 rust_proxy 实际部署机（VPS）上运行最准——很多大陆 CDN 会按地域封锁，
# 开发机/沙箱可能连不上（DNS 失败或超时）。
#
# 输出每行字段:
#   片=总分片数  disc=不连续标记数  cue=CUE-OUT/IN数
#   主流占比=最大同目录分片占比  偏离片=不在主流目录的分片数(同路径广告强信号)
# 判定:
#   ★有广告 (cue>0 或 偏离片>0) / ?有不连续(同路径,看是否短pod夹在大块间) / 干净
#
set -u
UA='Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36'

api="${1:-}"; want="${2:-6}"
if [ -z "$api" ]; then
  echo "用法: $0 <采集站API地址> [视频数=6]" >&2
  exit 2
fi

dir="$(mktemp -d)"
trap 'rm -rf "$dir"' EXIT

# 抓两页列表增加视频多样性
: > "$dir/all.json"
for pg in 1 2; do
  curl -s --max-time 18 "${api}?ac=detail&pg=$pg" -H "User-Agent: $UA" >> "$dir/all.json" 2>/dev/null
done
[ -s "$dir/all.json" ] || { echo "列表不可达: $api"; exit 0; }

# 不同视频的直链 m3u8（反转义后按完整 URL 去重）
sed 's/\\\//\//g; s/\\u0026/\&/g' "$dir/all.json" \
  | grep -oE 'https?://[a-zA-Z0-9._:/-]+\.m3u8[a-zA-Z0-9._:/?=&-]*' \
  | awk '!seen[$0]++' | head -"$want" > "$dir/vids.txt"
[ -s "$dir/vids.txt" ] || { echo "play_url 非直链 m3u8（播放器页/加密），本脚本无法解析"; exit 0; }

analyze_media() {
  local f="$1" url="$2" nseg ndisc ncue
  nseg=$(grep -cE '\.ts|\.jpe?g|\.png|\.mp4' "$f")
  ndisc=$(grep -cE '#EXT-X-DISCONTINUITY' "$f")
  ncue=$(grep -ciE 'CUE-OUT|CUE-IN' "$f")
  awk -v nseg="$nseg" -v ndisc="$ndisc" -v ncue="$ncue" '
    /\.ts|\.jpe?g|\.png|\.mp4/ && !/^#/ { d=$0; sub(/\?[^/]*$/,"",d); sub(/[^/]*$/,"",d); cnt[d]++ }
    END {
      best=0; for (k in cnt) if (cnt[k]>best) best=cnt[k]
      off=nseg-best
      verdict=(ncue>0 || off>0) ? "★有广告" : (ndisc>0 ? "?有不连续(同路径)" : "干净")
      printf "  片=%-5d disc=%-3d cue=%-2d 主流占比=%3.0f%% 偏离片=%-4d -> %s\n", \
             nseg, ndisc, ncue, (nseg?best*100.0/nseg:0), off, verdict
    }' "$f"
}

i=0
while read -r u; do
  [ -z "$u" ] && continue
  i=$((i+1))
  curl -s --max-time 12 "$u" -H "User-Agent: $UA" -H "Referer: ${u%/*}/" -o "$dir/m.m3u8" 2>/dev/null
  [ -s "$dir/m.m3u8" ] || { echo "  #$i 不可达: $u"; continue; }
  # master? 跟一层到 media
  if grep -q '#EXT-X-STREAM-INF' "$dir/m.m3u8"; then
    sub=$(grep -vE '^#' "$dir/m.m3u8" | grep -v '^[[:space:]]*$' | head -1)
    case "$sub" in
      http*) su="$sub";;
      /*)    su="$(echo "$u" | grep -oE '^https?://[^/]+')$sub";;
      *)     su="${u%/*}/$sub";;
    esac
    curl -s --max-time 12 "$su" -H "User-Agent: $UA" -H "Referer: ${u%/*}/" -o "$dir/m.m3u8" 2>/dev/null
  fi
  if [ -s "$dir/m.m3u8" ] && grep -q '#EXTINF' "$dir/m.m3u8"; then
    printf '#%d %s\n' "$i" "$u"
    analyze_media "$dir/m.m3u8" "$u"
  else
    echo "  #$i 无法解析到 media 播放列表"
  fi
done < "$dir/vids.txt"
