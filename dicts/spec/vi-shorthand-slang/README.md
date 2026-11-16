# SmartKey — Vietnamese Shorthand & Slang Specification

Production curation of the two Vietnamese lists SmartKey ships with, plus the
ambiguous/excluded audit trail. Files:

| File | Role |
|---|---|
| `shorthand_vi.tsv` | Bucket 1 — auto-expansion (trigger → phrase on Space/punctuation, Backspace restores trigger) |
| `slang_vi.txt` | Bucket 2 — pure shield: one token per line, kept verbatim, never expanded or autocorrected |
| `audit_ambiguous.tsv` | Machine-readable audit of every popular abbreviation intentionally excluded, with the exact reason |

**Lean-dict principle:** expansions are limited to triggers that are both
high-frequency and unambiguous (>90% one reading). Everything else is shielded
or dropped — a wrong expansion is worse than no expansion, and a rare entry
just pollutes the dictionary.

---

## Section 1 — Sources & Methodology

**Datasets / corpora actually used:**

- [ViSoLex](https://github.com/HaDung2002/visolex) — open Vietnamese social-media
  lexical-normalization system ([paper](https://arxiv.org/html/2501.07020v1)).
  Its `dict/dictionary.json` (3,091 NSW → standard-form entries) was downloaded
  and every candidate trigger was checked against it. It settled the contested
  calls: `nc` (nước *and* nói chuyện), `bt` (bình thường/biết/bài tập),
  `lm` (làm/lắm), `dag` (đường), `bx` (bến xe), `ks` (kỹ sư), `sp` (sư phạm),
  `cmn` (con mẹ nó), `bjo` (bao giờ), `t`/`m` (many readings).
- [ViLexNorm](https://github.com/ngxtnhi/vilexnorm) (EACL 2024; 10,467 annotated
  comment pairs, CC BY-NC-SA 4.0) — reference corpus for the normalization task
  and error taxonomy. Also mirrored at [SEACrowd/vilexnorm](https://huggingface.co/datasets/SEACrowd/vilexnorm).
- [ViSoNorm](https://github.com/AnhHoang0529/visonorm) and
  [Small-LexNormViHSD](https://github.com/AnhHoang0529/Small-LexNormViHSD) —
  supporting normalization toolkits/corpora consulted for coverage.

**Macro-table lineage (format & precedent, not copied):** Unikey
(`unikey.org`) and EVKey macro/"sổ tay gõ tắt" tables, and
[Bamboo Engine](https://github.com/BambooEngine/ibus-bamboo) — reviewed for
what stock IME macro tables cover and what they get wrong (they happily map
ambiguous triggers; SmartKey deliberately does not).

**IME context:** [fcitx5-lotus](https://github.com/LotusInputMethod/fcitx5-lotus)
(Telex + VNI, fcitx5 addon for Linux) runs alongside SmartKey's English
keyboard-us mode.

**Methodology:**

1. Assemble candidate triggers from the corpora above + everyday VN chat usage.
2. Verify each against ViSoLex; any trigger with a second live reading fails
   Rule 2 and is shielded instead (this overrode two spec examples: `nc`, `bt` —
   documented in Section 4).
3. Rule 1 collision check: no trigger may be a valid standalone Vietnamese
   syllable (checked against the VN syllable inventory) or a common English
   word (checked against a top-English-words list; user-forbidden list included).
4. Rule 3: cringe/baby-talk teencode excluded entirely (audit table).
5. Rule 4 Telex audit: every trigger containing `s f r x j w` or `dd` is
   flagged `telex:compose` (lotus consumes it before commit) or `telex:safe`
   (modifier key present but no vowel before it → cannot compose).
6. Lean-dict pass (user directive): corpus-clean but low-frequency candidates
   (`stk`, `gt`, `pv`, `yk`, `tkb`, `gvcn`, `thpt`, `thcs`, `cmnd`, `cccd`,
   `gpkd`, `hdsd`, `qtv`, `bds`, `xly`, `qd`, `qg`, `cq`, `gđ`, `xđ`, `trk`,
   `nta`, `mih`, `kcg`, `bjh`, `dth`, `đth`) were dropped — see audit table.

**Runtime behavior notes for SmartKey:**

- Trigger matching is case-insensitive; mirror capitalization onto the
  expansion (`Ko → Không`, `KO → KHÔNG`). Expansion for `vn`/`sđt`-class
  proper nouns uses canonical sentence case (`vn → Việt Nam`).
- One Backspace immediately after expansion deletes the whole phrase and
  restores the raw trigger.
- Expansion fires on Space or punctuation; the punctuation itself is not eaten.
- Triggers are word-bounded tokens: `bh` fires, `30bh` or `bh123` does not.

---

## Section 2 — shorthand_vi.tsv (57 entries)

Same content as the TSV; `# notes` reproduce the file's third column.

### 1. Core verbs, negations, and particles (17)

```
ko→không  dc→được  đc→được  ddc→được  cx→cũng  ms→mới  nx→nữa
kp→không phải  kdc→không được  kcj→không có gì  chx→chưa  ns→nói
nma→nhưng mà  nhma→nhưng mà  j→gì  gj→gì  s→sao
```

### 2. Pronouns and people (9)

```
mk→mình  mn→mọi người  mng→mọi người  ng→người  ngta→người ta
ny→người yêu  nyc→người yêu cũ  vk→vợ  cj→chị
```

### 3. Time, question, and connector phrases (9)

```
ntn→như thế nào  trc→trước  bh→bây giờ  hqua→hôm qua  hnay→hôm nay
bnh→bao nhiêu  tg→thời gian  tgian→thời gian  ncl→nói chung là
```

### 4. Everyday phrases, study, work, and communication (22)

```
bth→bình thường  bthg→bình thường  vv→vui vẻ  yy→yêu  sn→sinh nhật
sk→sức khỏe  bs→bác sĩ  sdt→số điện thoại  sđt→số điện thoại  cty→công ty
kd→kinh doanh  ql→quản lý  sl→số lượng  sd→sử dụng  qc→quảng cáo
kq→kết quả  klq→không liên quan  mxh→mạng xã hội  cskh→chăm sóc khách hàng
vn→Việt Nam  nt→nhắn tin  trl→trả lời
```

**Telex (fcitx5-lotus) flag summary for triggers:**

| Flag | Triggers | Meaning |
|---|---|---|
| `telex:compose` | `ddc`, `gj`, `cj` | lotus usually consumes these before commit: `ddc→đc`, `gj→gì`, `cj→cì`(!). They exist as ASCII-mode fallbacks; `cj→chị` only reachable when Telex is off. |
| `telex:safe` (modifier key present, cannot compose) | `ms`, `nx`, `chx`, `ns`, `j`, `s`, `trl` (`r` here is the **tr** onset), `ddc`/`đc` family (`dd→đ`) | key `s/f/r/x/j/w` appears but there is no vowel before it inside the token, so lotus never transforms it; SmartKey must still only fire on standalone word-bounded tokens. |

---

## Section 3 — slang_vi.txt (189 tokens, pure shield)

Plain UTF-8, one token per line, no comments (grouping below is documentation
only). Kept verbatim: never expanded, never autocorrected by the statistical
EN/VI corrector.

**A. Chat loanwords & digital/shopping actions (41)**
`ib, iv, rep, cmt, stt, st, fl, cf, cafe, fb, ig, yt, ytb, tt, tw, zl, gg, acc, id, pass, link, bill, ship, shipper, cod, ord, od, deal, sale, freeship, slot, ad, mod, mem, ot, hr, qa, pc, pr, momo, wa`

**B. Conversational acknowledgments & interjections (35)**
`ừ, ừm, ờ, ủa, uầy, uh, uhm, uk, ok, oke, oki, okie, okela, oce, hm, hmm, um, haha, hihi, hehe, hjhj, kkk, kk, huhu, mlem, gato, gu, tui, rì, nịt, xíu, ghệ, ghe, ngu, bb`

**C. Intensifiers & profanity shields (19)**
`vãi, vl, vll, vcl, vkl, vcc, vch, cl, clgt, clm, dm, đm, dcm, dkm, dmm, ml, cc, qq, cmn`

**D. Teencode letter/particle shields (10)**
`z, zj, zị, zô, rj, wj, nh, nhe, ne, ui`

**E. Ambiguous Vietnamese abbreviations — shielded, never guessed (84)**
`đk, dk, ddk, đt, dt, tk, hs, sv, gv, nv, gd, bm, vs, kh, kg, hn, hq, tc, td, hd, tb, vp, cv, hp, mb, km, bd, bq, bl, cb, nc, tn, bn, nn, nch, qt, qtr, cn, dh, đh, đg, dg, dag, dd, đr, xd, vt, vc, tx, ths, ty, cm, ct, pb, cs, xh, ht, ts, tm, pm, bv, kc, hx, ks, sp, bx, lm, bjo, bt, sg, hcm, dn, pq, dl, bk, bc, cp, k, r, v, w, x, f, u`

Note the deliberate asymmetries: `dc/đc/ddc` expand to **được** while `đg/dg/dag`
are shielded (đang/đường); `vd`-class triggers stay out; bare letters
`k r v w x f u` are shielded because they are simultaneously VN teencode
particles and Telex modifier keys.

---

## Section 4 — Ambiguous / Excluded Vietnamese Shorthand Audit Table

Full machine-readable version: `audit_ambiguous.tsv`. Verdicts:
**shield** = in `slang_vi.txt`; **dropped** = in no list.

### 4.1 Polysemous abbreviations → shielded (never guessed)

| Trigger | Candidate meanings | Why excluded |
|---|---|---|
| đk / dk / ddk | đăng ký / điều kiện / được không | 3 live readings |
| đt / dt | điện thoại / đội tuyển / dân tộc / data | 3–4 readings |
| tk | tài khoản / thiết kế | 2 readings |
| hs | học sinh / hồ sơ | 2 readings |
| sv | sinh viên (dominant) | role-abbrev; spec says keep verbatim |
| gv | giáo viên / giúp việc | 2 readings |
| nv | nhân viên / nhiệm vụ | 2 readings |
| gd | gia đình / giao dịch | 2 readings (`gđ` variant dropped: low usage) |
| bm | bố mẹ / bộ môn | 2 readings |
| vs | với / versus | EN collision + VN preposition |
| kh | không / khách hàng | 2 readings |
| kg | không / kilôgram | unit collision |
| hn | hôm nay / Hà Nội | time vs city |
| hq | hôm qua / Hàn Quốc / hải quân | 3 readings (ViSoLex) |
| tc | tài chính / tổ chức / tình cảm | 3 readings (ViSoLex) |
| td | thể dục / tiến độ / tự do | 3 readings (ViSoLex) |
| hd | hướng dẫn / hành động / high-definition | 3 readings |
| tb | thông báo / tân binh / trung bình | 3 readings (ViSoLex) |
| vp | văn phòng / vật phẩm | 2 readings |
| cv | công việc / CV (curriculum vitae) | VN reading vs English resume |
| hp | hạnh phúc / Hải Phòng / học phí / hit points | 3+ readings (ViSoLex) |
| mb | máy bay / megabyte | unit collision |
| km | khuyến mãi / kilômét | unit collision |
| bd | bóng đá / birthday / Bình Dương | 3 readings |
| bq | bỏ qua / bình quân / bảo quản | 3 readings (ViSoLex) |
| bl | bình luận / block / Bạc Liêu | "bl t" genuinely 2-way |
| cb | chuẩn bị / cán bộ / cơ bản / các bạn | 4 readings (ViSoLex) |
| **nc** | **nước / nói chuyện** | **overrides spec example** — ViSoLex shows a real 2-way split ("nc ngọt" vs "nc với t") |
| tn | thế nào / tin nhắn / trắc nghiệm | 4 readings (ViSoLex) |
| bn | bạn / bao nhiêu / bệnh nhân | 4 readings (ViSoLex) |
| nn | ngủ ngon / nghỉ ngơi / ngôn ngữ / người nước ngoài | 3–4 readings |
| nch | nói chung / nói chuyện | 2 readings (ViSoLex) |
| qt | quan trọng / quá trình / quốc tế / quan tâm | 4 readings |
| qtr | quá trời / quá trình / quan trọng | 3 readings (ViSoLex) |
| cn | chủ nhật / công nghệ | 2 readings (ViSoLex) |
| dh / đh | đại học / đơn hàng | student vs seller chat |
| đg / dg / dag | đang / đường | ViSoLex: dag→đường; "đg 30/4" vs "đg ăn" |
| đr | đúng rồi / được rồi | 2 readings |
| xd | xác định / “XD” emoticon | lowercase emoticon collides |
| vt | viết / vô tri | ViSoLex says viết, but “vt thật” slang is live |
| vc | vợ chồng / vãi cả / việc / vụ | 4 readings (ViSoLex) |
| tx | thanks / tài xế | 2 readings (ViSoLex) |
| ths | thanks / thông số | 2 readings |
| ty | thank you / tỉ / tình yêu | 3 readings (ViSoLex) |
| cm | cảm ơn / comment / centimet / chứng minh / chúng mày | 4+ readings (ViSoLex) |
| ct | chi tiết / chương trình / chia tay | 3 readings (ViSoLex) |
| pb | phiên bản / playboy | 2 readings |
| cs | chăm sóc / cuộc sống / Counter-Strike | 3 readings (ViSoLex) |
| xh | xã hội / xuất hiện | 2 readings |
| ht | hiện tại / hệ thống / hết | 3 readings (ViSoLex) |
| ts | trà sữa / tiến sĩ / tới / tự sướng | 3+ readings (ViSoLex) |
| tm | thôi mà / trademark / tạm | 2 readings + ™ |
| pm | phần mềm / private message | 2 readings |
| bv | bệnh viện / bảo vệ | 2 readings (ViSoLex) |
| kc | không có / kiểm soát | 2 readings |
| wa | qua / quá / WhatsApp | 2 VN readings + brand (ViSoLex) |
| hx | hàng xóm / hồi xưa | 2 readings (ViSoLex) |
| ks | khách sạn / kỹ sư | 2 readings (ViSoLex) — below expansion bar |
| sp | sản phẩm / sư phạm | 2 readings (ViSoLex) |
| bx | bà xã / bến xe | ViSoLex says bến xe; 2-way |
| **lm** | **làm / lắm** | “lm gì” vs “xinh lm” — ViSoLex shows both; shielded |
| **bt** | **bình thường / biết / bài tập** | **overrides spec example** — ViSoLex shows a 3-way split |
| bjo | bây giờ / bao giờ | ViSoLex says bao giờ; 2-way |
| cmn | chúc mừng / con mẹ nó | expanding would inject profanity into birthday wishes |
| qt-cousin tnao | thế nào / tên nào | 2-way |
| ls | lịch sử / Linux `ls` | Linux-host collision |
| mon | môn (ASCII) | corrector would “fix” to moon/mon |
| hđ | hoạt động / hợp đồng | 2 readings |
| cđ / cd | cao đẳng / cuộc đời / CD | 2–3 readings |
| sg, hcm, dn, pq, dl | city codes | proper-noun codes kept verbatim |
| bk, bc, cp | Bách Khoa/bảo kê/book · bồ cũ/bình chọn · chi phí/couple | 2–3 readings each |

### 4.2 Bare letters & particles → shielded or dropped

| Trigger | Verdict | Reason |
|---|---|---|
| k, r, v, w, x, f, u, z | shield | single letters doubling as VN particles AND Telex modifier keys (`s f r x j w`); expanding would fire on ordinary letter typing |
| j, gj | expand (gì) | clean per ViSoLex; `gj` only reachable with Telex off |
| t | dropped | ViSoLex: tôi/thằng/tớ/tao/tuổi… — too many readings |
| m | dropped | ViSoLex: mày/mình/mẹ/má/mi… — too many readings |
| e | dropped | `e.g.` false positive + generic letter |
| a, i | dropped | hard English collisions (“a”, “I”) |
| b, d, g, h, n, p, q | dropped | no dominant reading / unit-style usage / Telex key (`d`) |

### 4.3 Cringe / baby-talk teencode → dropped (Rule 3)

`iu, ik, khum, hok, hem, hong, hum, bít, rùi, lun, quài, zậy, dzậy, zn, zui,
dzui, wá, qá, thui, nhìu, zê, jz, chằm zn`

### 4.4 Corpus-clean but dropped for leanness (user directive)

`stk, gt, pv, yk, tkb, gvcn, thpt, thcs, cmnd, cccd, gpkd, hdsd, qtv, bds,
xly, qd, qđ, qg, cq, gđ, xđ, trk, nta, mih, kcg, bjh, dth (ViSoLex: dễ thương!),
đth`

### 4.5 Spec-example deviations (evidence-based)

1. **`nc → nói chuyện` was NOT adopted.** ViSoLex lists `nc → nước` and
   `nc → nói chuyện` with both live in chat (“nc ngọt”, “uống nc” vs “nc với t”).
   Per Rule 2 it is shielded + audited instead.
2. **`bt → bình thường` was NOT adopted.** ViSoLex lists `bt → bình thường /
   biết / bài tập`. `bth/bthg` (clean) expand instead; `bt` is shielded.
3. Everything else from the spec examples was verified and adopted
   (`ko, dc, đc, ntn, trc, bh, cx, ms, mk, mn, ns, hqua, hnay, ngta` — all
   confirmed unambiguous by ViSoLex), plus `vđ` was dropped as low-usage.

---

## Change control

- Adding a trigger requires: ViSoLex/corpus single-reading evidence + syllable
  inventory pass + English top-words pass + Telex flag.
- Any trigger found colliding after ship: move to `slang_vi.txt` and log an
  audit row; never delete silently.
