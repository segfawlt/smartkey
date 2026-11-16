# SmartKey — English Shorthand & Slang Specification (v1.1)

Machine lists: [`shorthand_en.tsv`](shorthand_en.tsv) (57 entries) · [`shorthand_en_extended.tsv`](shorthand_en_extended.tsv) (31 opt-in entries) · [`slang_en.txt`](slang_en.txt) (186 tokens).
This spec supersedes and extends the 75-line baseline `dicts/en/slang.txt`.

**v1.1 — frequency trim.** The default expansion pack was cut from 89 → 57 triggers and the shield from 222 → 186 tokens by removing variants, niche-register, seasonal, and declining-meme entries. Trimmed triggers are preserved in the opt-in extended pack. See [Changelog](#changelog).

---

## 1. Sources & Methodology

### Sources cross-referenced

| Source | What was taken from it |
|---|---|
| [espanso/espanso](https://github.com/espanso/espanso) + [Espanso Hub](https://espanso.org/hub) + [jpmvferreira/espanso-mega-pack](https://github.com/jpmvferreira/espanso-mega-pack) | Trigger→expansion semantics, abbreviation pack shapes, firing-on-word-boundary behavior |
| Jim Biancolo's 2007 `AutoCorrect.ahk` ([endolith gist mirror](https://gist.github.com/endolith/876629), maintained as [kunkel321/AutoCorrect2](https://github.com/kunkel321/AutoCorrect2)) | Hotstring-style shorthand culture; typo-vs-shorthand separation (typos belong to the corrector, not the expander) |
| [anubhavsrivastava/awesome-internet-abbreviation-acronyms](https://github.com/anubhavsrivastava/awesome-internet-abbreviation-acronyms) | Broad SMS/chat initialism inventory used as recall checklist |
| [shreyanshu28/slang-translator](https://github.com/shreyanshu28/slang-translator) + [averz/genz-slang-dataset](https://huggingface.co/datasets/averz/genz-slang-dataset) | Formal↔informal mapping pairs, Gen Z token recall (rizz, gyat, delulu, bussin, nocap…) |
| Wikipedia "SMS language" tables, NetLingo texting lists, Wiktionary internet-slang categories | Historical coverage; used mainly to enumerate what to EXCLUDE (Rule 4 leet) |
| GitHub / Slack / Discord / Twitch live conventions | lgtm, ptal, wip, nit, iiuc, hth, tfw, mfw, ftfy, pog/poggers, copium, sadge, kekw, o7 |
| Baseline [`dicts/en/slang.txt`](../../dicts/en/slang.txt) in this repo | Every existing entry audited and re-bucketed (see §4 rows 43–45) |
| Register references | American Dialect Society WOTY 2023 "rizz"; Collins/Oxford Gen Z additions (delulu, situationship era); macOS/iOS text-replacement exports for casing conventions |

### Curation rules applied

1. **Rule 1 — no real-word triggers.** No `shorthand_en.tsv` trigger is a standalone English word. This also bans the whole tech-token family (`git vim ssh cli api url sql css xml pic doc app net web bot mod dev ops log bug fix`) and function words (`in on am us so it he me be do to go no or as at by up an if is my of we hi ok ad id ex pi pro con min max sec`).
2. **Rule 2 — one expansion only.** A trigger is admitted only if >90% of everyday typing maps to one expansion. Everything split across domains (`ig`, `fr`, `sm`, `np`, `ty`, `pm`, `dm`, `eta`, `tbd`, `tba`, `atm`, `gtg`…) is shield-only and listed in §4.
3. **Rule 3 — no typos, no missing apostrophes.** `teh adn wat wut gud recieve definately` etc. stay with the typo corrector. `im ive id ill dont cant wont didnt doesnt isnt arent wasnt werent havent hasnt hadnt wouldnt couldve shouldve youre theyve weve` etc. belong to the dedicated contraction engine. Phonetic spoken contractions (`gonna finna tryna ima y'all ain't c'mon`) are shielded because users type them intentionally for tone.
4. **Rule 4 — no 2000s leet.** `l8r gr8 b4 2day 2nite cya kthx pwn n00b rawr xD roflmao ttfn bak ltns sm1 m8 w8 u ur r` are all excluded (§4).
5. **v1.1 frequency bar.** The default pack keeps only triggers with high everyday frequency on desktop. Variants of an included trigger, niche-register tokens (Reddit-only, mobile-DM-only, UK-only), seasonal one-offs, and declining memes were demoted to the extended pack or dropped. The shield keeps only tokens that are either frequent or genuinely mangle-prone.

### Engine conventions the lists assume

- **Firing:** a trigger expands when terminated by Space, Enter, or terminal punctuation (`. , ! ? ; :`) at a word boundary. Expansion preserves trailing punctuation.
- **Undo:** one BackSpace immediately after expansion deletes the whole expansion and restores the raw trigger (`pls`), exactly per SmartKey's expander contract.
- **Casing policy (resolves most caps ambiguities):**
  - lowercase trigger → lowercase expansion (`pls ` → `please `)
  - Sentence-case trigger → sentence-cased expansion (`Pls ` → `Please `)
  - **ALL-CAPS trigger → NO expansion** (`BC`, `GM`, `JW`, `FAQ` pass through untouched). This single policy neutralizes the era-date / game-master / religion readings of otherwise-dominant lowercase triggers.
  - Expansions capitalize `I` and proper nouns only (`I don't know`, `I swear to God`); everything else is lowercase so SmartKey can project casing.
- **Punctuation edge:** `ikr → "I know, right?"` carries a terminal `?`. If fired on `?`, the engine must collapse the doubled `?` to one.
- **Vulgarity:** exactly two TSV rows are profane (`idgaf`, `idfk`). Ship them behind the pack's profanity flag; every other profane form is shield-only (never auto-typed by SmartKey).
- **Lengthened emphasis forms** (`plss`, `thxxx`, `lmaooo`, `nooo`) are intentionally absent from both lists — extra letters are emphasis and must never be touched.

---

## 2. List 1 — `shorthand_en.tsv` (auto-expansion, 57 entries)

Format: `trigger → expansion # note`. The raw file is strictly `trigger<TAB>expansion`.

### 2.1 Group A — conversational single-word shortenings (12)

```
pls    → please      # "plss" emphasis stays untouched
plz    → please      # variant of pls
thx    → thanks      # "thxxx" emphasis stays untouched
sry    → sorry       # srry variant demoted to extended pack
ppl    → people
msg    → message
bday   → birthday    # greeting form is hbd
tmr    → tomorrow    # digit forms (2moro, 2morrow) excluded per Rule 4
tmrw   → tomorrow    # longer variant of tmr
bc     → because     # caps "BC" (era date) never expands; note: `bc` is also a shell calculator name — trigger only fires in prose word context
fav    → favorite    # tone form "fave" is a dictionary word, no shield needed
smth   → something   # extremely common 2020s chat filler
```

### 2.2 Group B — multi-word conversational initialisms (45)

```
# --- status, availability & replies ---
rn     → right now
brb    → be right back
omw    → on my way
otw    → on the way
ttyl   → talk to you later
nvm    → never mind               # two-word form is the standard spelling
dw     → don't worry
nbd    → no big deal
yw     → you're welcome
wbu    → what about you
hbu    → how about you

# --- opinions & framing ---
tbh    → to be honest
tbf    → to be fair
imo    → in my opinion
imho   → in my humble opinion
ngl    → not gonna lie
ikr    → I know, right?           # terminal "?" — collapse doubled "?" when fired on "?"
istg   → I swear to God
jw     → just wondering           # caps "JW" (Jehovah's Witness) never expands
jic    → just in case
ofc    → of course
fwiw   → for what it's worth

# --- knowledge, questions & comprehension ---
idk    → I don't know
idc    → I don't care
idgaf  → I don't give a fuck      # VULGAR — gate behind profanity flag
idfk   → I don't fucking know     # VULGAR — gate behind profanity flag
afaik  → as far as I know
iirc   → if I recall correctly
iiuc   → if I understand correctly # GitHub/Slack review register
wdym   → what do you mean
wdyt   → what do you think        # review-request register
wyd    → what are you doing
lmk    → let me know

# --- gratitude & affection ---
tysm   → thank you so much
ily    → I love you
imy    → I miss you
hbd    → happy birthday
gm     → good morning             # caps "GM" (game master) never expands
gn     → good night

# --- meta, forum & handoff ---
btw    → by the way
fyi    → for your information
icymi  → in case you missed it
hth    → hope this helps          # "hope that helps" = same function
iykyk  → if you know, you know    # Gen Z staple
tmi    → too much information
```

### 2.3 Extended pack — `shorthand_en_extended.tsv` (31 opt-in entries)

Same curation rules, cut only on frequency: SMS-flavored variants (`thxs srry bcuz abt rly btwn tomoz msgs`), workplace one-offs (`appt dept`), variants of included triggers (`tbqh stg wym tyvm ilysm`), niche-register tokens (`idr idts idgi idrc afaict iykwim hru wsp wsg wya`), seasonal/low-frequency (`brt bbl bfn tyt gws hny`).

---

## 3. List 2 — `slang_en.txt` (pure shield: never expanded, never autocorrected, 186 tokens)

### 3.1 Chat reactions & internet acronyms (38)

Length/tone IS the meaning — expanding any of these feels robotic. `tl;dr` and `tldr` are both listed so either spelling is protected.

```
lol
lmao
lmfao
rofl
kek
kekw
haha
hehe
smh
omg
omfg
wtf
wth
ffs
tf
af
ftw
tldr
tl;dr
jk
jfc
fml
gtfo
bs
stfu
dgaf
oof
welp
sheesh
yikes
meh
eww
aww
woah
uwu
kk
mhm
ugh
```

### 3.2 Spoken informal contractions kept verbatim (34)

Typed intentionally for tone; the contraction engine must never "fix" them, and `tho/thru/woah` variants must survive the corrector.

```
gonna
wanna
gotta
kinda
sorta
outta
lotta
lemme
gimme
dunno
prolly
ima
finna
tryna
boutta
gotcha
whatcha
shoulda
coulda
woulda
y'all
ain't
c'mon
innit
cuz
em
def
defo
obvi
totes
tho
thru
sup
soz
```

### 3.3 Modern conversational slang & tone markers (33)

Active 2020s register (Gen Z / Twitch / Discord / TikTok): `rizz` (ADS WOTY 2023), `gyat`, `delulu`, `bussin`, `nocap`, `mid`, `deadass`, `ong`, `oomf`, `gng`, plus core agreement/reaction tokens.

```
bruh
bruv
blud
cuh
fam
nah
yep
yup
nope
yea
yeah
sus
goat
goated
mid
cap
nocap
vibe
vibes
lowkey
highkey
deadass
bet
lit
bae
rizz
gyat
delulu
bussin
yeet
ong
gng
oomf
```

### 3.4 Workplace, gaming & social abbreviations kept verbatim (66)

Expanding any of these in context would be wrong or robotic: `lgtm/wip/ptal/nit` (code review), `eod/eow/eoy/cob/ooo/pto/wfh` (workplace), `gg/ggwp/glhf/afk/ez/o7/p2w/pog/poggers/copium/sadge/gz/grats/nt` (gaming), `dm/dms/pm/vc/pfp/hmu/wb/tfw/mfw/ftfy` (social), the dev-token set (`docs/deps/req/impl/pkg/repo/env/func/cmd/auth/creds/perf/wrt`) that the corrector would otherwise mangle (`func`→`funk`, `env`→`even`), and `bff/bf/gf/bb`.

```
lgtm
wip
ptal
nit
diy
pov
irl
afk
gg
ggwp
glhf
gl
hf
wp
nt
ez
o7
p2w
pog
poggers
copium
sadge
vc
dm
dms
pm
pfp
hmu
wfh
eod
eow
eoy
cob
ooo
pto
eta
tbd
tba
asap
rsvp
faq
aka
mvp
bff
bf
gf
bb
gz
grats
docs
deps
req
impl
pkg
repo
env
func
cmd
auth
creds
wb
tfw
mfw
ftfy
perf
wrt
```

### 3.5 Ambiguous shorthands shielded from false autocorrect (15)

Same tokens documented in §4 — shielded so the statistical corrector never mangles them into dictionary words, but never expanded because context decides the meaning.

```
ig
fr
frfr
sm
np
ty
ik
gtg
prob
nm
atm
ot
op
ic
pr
```

---

## 4. Ambiguous / Excluded English Shorthand Audit Table

Every popular abbreviation deliberately kept **out** of `shorthand_en.tsv`, with the exact reason. "Placement" = where it ended up (or why it was dropped entirely).

| Trigger(s) | Conflicting readings / collision | Placement | Why excluded from auto-expansion |
|---|---|---|---|
| `ig` | I guess · Instagram | slang_en.txt | Platform/brand collision; expansion breaks hashtag & handle reads |
| `fr` | for real · French · frame | slang_en.txt | Cross-domain split; also deliberate emphasis cadence |
| `frfr` | for real for real | slang_en.txt | Doubled emphasis is the register itself |
| `sm` | so much · some · social media | slang_en.txt | Three-way split |
| `np` | no problem · NumPy · NP (complexity class) | slang_en.txt | Dev/math collision |
| `ty` | thank you · Ty (given name) | slang_en.txt | Common given name; 2-letter false-trigger risk |
| `ik` | I know · IK (inverse kinematics) | slang_en.txt | Gamedev/robotics collision |
| `atm` | at the moment · automated teller machine | slang_en.txt | Canonical split |
| `eta` | estimated time of arrival · edited to add | slang_en.txt | Forum/Reddit split |
| `tbd` | to be determined · to be discussed | slang_en.txt | Formal initialism; split |
| `tba` | to be announced · to be advised | slang_en.txt | Split + formal register |
| `nm` | nothing much · never mind · not much · nanometer | slang_en.txt | Three-way split |
| `gtg` | got to go · good to go | slang_en.txt | Opposite-direction meanings |
| `ot` | off topic · overtime · occupational therapy | slang_en.txt | Split |
| `op` | original poster · overpowered · "op" (dict word) | slang_en.txt | Split + Rule 1 real word |
| `oc` | original content · of course · OOC | dropped (v1.1) | Split + low frequency |
| `ic` | I see · integrated circuit | slang_en.txt | Split + 2-letter false-trigger risk |
| `pr` | pull request · public relations · Puerto Rico | slang_en.txt | Split |
| `mr` | merge request · Mister (Mr.) | dropped | Honorific collision; corrector leaves it intact |
| `wb` | welcome back · write back · what about | slang_en.txt | Split |
| `dyk` | did you know · do you know | dropped | Tense split, low expansion payoff |
| `bf`, `gf`, `bff` | boyfriend · girlfriend · best friend(s) forever | slang_en.txt | Intentional endearments; bff has singular/plural split |
| `bb` | baby · be back · basketball | slang_en.txt | Split |
| `prob` | probably · problem(s) ("no prob") | slang_en.txt | Noun/verb split |
| `gfy` | good for you · go fuck yourself | dropped | Hostile reading; auto-expansion risk unacceptable |
| `kys`, `kms` | kill yourself · kill myself | dropped | Self-harm acronyms: never expanded, never shielded |
| `k` | okay | dropped | Single-letter trigger, out of scope |
| `w`, `l`, `f` | win · loss · pay respects | dropped | Single-letter triggers, out of scope |
| `u`, `ur`, `r`, `y` | you · your/you're · are · why | dropped | Rule 4 letter substitution; collides with pronouns/prepositions |
| `m8`, `w8`, `gr8`, `l8r`, `b4`, `2day`, `2nite`, `sm1` | digit leet (mate, wait, great, later, before, today, tonight, someone) | dropped | Rule 4 — obsolete SMS leet |
| `cya`, `kthx`, `pwn`, `n00b`, `rawr`, `xD`, `roflmao`, `ttfn`, `bak`, `ltns` | dated 2000s chat | dropped | Rule 4 |
| `xmas` | Christmas | dropped | Dictionary word (Rule 1); intentional spelling needs no correction |
| `ion` | I don't (AAVE) · ion (physics) · "iron" typo | dropped | Real word; shielding would block iron↔ion typo repair |
| `sup`, `em` | what's up · 'em (them) | slang_en.txt | Near-dictionary tokens shielded as greeting/clitic register |
| `tho`, `thru`, `woah`, `cuz`, `def`, `obvi`, `totes`, `prolly` | intentional phonetic spellings | slang_en.txt | Expansion would strip tone; corrector must not "repair" them |
| `pic`, `pics`, `doc`, `app`, `net`, `web`, `bot`, `mod`, `dev`, `ops`, `log`, `bug`, `fix`, `info` | entrenched tech/dictionary words | dropped | Rule 1 |
| `git`, `vim`, `ssh`, `cli`, `api`, `url`, `sql`, `css`, `xml` | tech proper nouns/initialisms | dropped | Rule 1 — protected tech tokens |
| `min`, `max`, `sec`, `ad`, `id`, `ex`, `pi`, `pro`, `con`, `ok`, `hi`, `me`, `us`, `it`, `in`, `on`, `am`, `be`, `do`, `to`, `go`, `no`, `or`, `as`, `at`, `by`, `up`, `an`, `if`, `is`, `my`, `of`, `we`, `he` | plain English dictionary words | dropped | Rule 1 — never usable as triggers |
| `im`, `ive`, `id`, `ill`, `dont`, `cant`, `wont`, `didnt`, `doesnt`, `isnt`, `arent`, `wasnt`, `werent`, `havent`, `hasnt`, `hadnt`, `wouldnt`, `couldve`, `shouldve`, `youre`, `youve`, `theyre`, `theyve`, `weve`, `its`, `thats`, `whats`, `lets` | missing-apostrophe contractions | dropped | Rule 3 — SmartKey's contraction engine owns these |
| `yall`, `cmon` | y'all / c'mon minus apostrophe | dropped | Rule 3 consistency (apostrophe forms ARE shielded) |
| `teh`, `adn`, `nad`, `wat`, `wut`, `gud`, `thnks`, `recieve`, `definately` | keyboard typos | dropped | Rule 3 — typo corrector's domain |
| `s/o`, `w/`, `w/o`, `b/c`, `c/o`, `n/a`, `y/n` | slash-form abbreviations | dropped | Non-word triggers; out of trigger-engine scope |
| `asap`, `rsvp`, `faq`, `aka`, `mvp`, `diy`, `pov`, `irl`, `afk`, `wfh`, `hmu`, `pfp`, `eod`, `eow`, `eoy`, `cob`, `wip`, `lgtm`, `ptal`, `nit`, `vc`, `dm`, `dms`, `pm` | workplace/social initialisms (pm: post meridiem · product manager · prime minister; dm: direct message · dungeon master; faq: expletive reading) | slang_en.txt | Kept verbatim by design — context-bound meanings and intentional register |
| `lol`, `lmao`, `lmfao`, `rofl`, `kek`, `kekw`, `smh`, `omg`, `omfg`, `ffs`, `ftw`, `tldr`, `tl;dr`, `jk`, `fml`, `gtfo`, `stfu`, `bs`, `dgaf`, `jfc`, `af`, `tf`, `wth` | reaction tokens | slang_en.txt | Length/tone IS the meaning; auto-expansion feels robotic and can misfire in hostile contexts |
| `bc`, `gm`, `gn`, `jw` | single dominant lowercase reading, but caps forms mean other things (BC era · GM game master · JW religion) | shorthand_en.tsv (lowercase only) | Admitted under the ALL-CAPS-never-expands policy, with notes |
| `perf`, `wrt` | intentional dev shorthand (performance, with regard to) | slang_en.txt | Devs type these verbatim in reviews/commits; expanding is unwanted (v1.1: moved from expander to shield) |

---

## Changelog

### v1.1 — frequency trim

- **`shorthand_en.tsv`: 89 → 57 entries.** Dropped 31 low-frequency triggers (preserved in [`shorthand_en_extended.tsv`](shorthand_en_extended.tsv)): `thxs srry msgs tomoz bcuz abt rly btwn appt dept tbqh brt bbl bfn tyt gws hny hru stg idr idts idgi idrc afaict wym iykwim wsp wsg wya ilysm tyvm`. Moved `perf` and `wrt` to the shield (intentional dev register — expanding is unwanted). Re-added `fwiw`, which was accidentally omitted from the v1.0 file despite being in the spec's Rule-2 example set.
- **`slang_en.txt`: 222 → 186 tokens.** Dropped 38 low-frequency variants and declining memes: `lel pmsl smfh smdh oml wtaf asf gdi smd owo pfft` (reactions), `musta oughta hafta coz deffo obvs altho` (contractions), `brah sussy smol birb doggo pupper ratioed fave yessir` (slang), `ggs ff pogchamp eom gratz vid reqs mrw` (workplace/gaming), `g2g probs oc` (ambiguous). Added `perf` and `wrt`.
- **`shorthand_en_extended.tsv`** added as the opt-in home for the trimmed triggers (31 entries).

### v1.0

- Initial specification: 89 expansion entries + 222 shield tokens, built from Espanso/AHK packs, chat-abbreviation datasets, and live Slack/Discord/GitHub/Twitch conventions; superseded the 75-line baseline `dicts/en/slang.txt`.

---

### Diff note vs. baseline `dicts/en/slang.txt`

The existing 75-line shield contains `ofc idk imo imho tbh ngl ikr afaik nvm rn pls plz thx btw fyi brb` and friends, which this spec **promotes to auto-expansion** (§2) — that is the intended upgrade path: merge §2 into the expander table, replace `slang.txt` with §3's superset, and keep `dev_terms.txt` as-is (it already covers most of the §3.4 dev tokens).
