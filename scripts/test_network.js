#!/usr/bin/env node
// luci-app-fm350 前端 AT 解析独立回归测试（审查人员自建，替代 PR 描述中未交付的 test_network.js）
//
// 运行：node scripts/test_network.js
// 覆盖：
//   1. GTCCINFO 六种布局的字段定位（服务/邻区 × LTE/NR/WCDMA）
//   2. GTACT 读写回显的位置参数解析（PreferredAct 不进频段列表）
//   3. 阶梯索引换算边界（0 / 255 / maxIdx / 126 / 127）
//   4. 频段编码解码（NR "50"+n / LTE 100+n / UMTS 1..10 / 非法值）
//   5. NR-ARFCN 两段栅格与重叠频段标注
//   6. 前后端一致性：本文件内嵌的 Rust 等价实现逐项对拍
'use strict';

var fs = require('fs');
var path = require('path');

var ROOT = path.resolve(__dirname, '..');
var SRC = fs.readFileSync(
  path.join(ROOT, 'htdocs/luci-static/resources/view/fm350/network.js'), 'utf8');

// 从 network.js 抽取纯函数区段（fmtNum .. parseGtact），eval 后逐项断言。
// 锚点刻意避开样式与 UI 代码：解析层全部在这两个标记之间。
var start = SRC.indexOf('function fmtNum');
var end = SRC.indexOf('function parseEmmchlock');
if (start < 0 || end < 0 || end < start) {
  console.error('抽取锚点失效，network.js 结构已变化');
  process.exit(2);
}
var SRC_SNIPPET = SRC.slice(start, end);
var ns = (0, eval)('(function(){ ' + SRC_SNIPPET + '\n return { ' +
  'idxToLinear: idxToLinear, rsrpFromIdx: rsrpFromIdx, rsrqFromIdx: rsrqFromIdx, ' +
  'sinrFromRaw: sinrFromRaw, nrBandNo: nrBandNo, nrBandName: nrBandName, ' +
  'lteBandName: lteBandName, bandName: bandName, bandGroup: bandGroup, ' +
  'arfcnToBandLabel: arfcnToBandLabel, gtccinfoLayout: gtccinfoLayout, ' +
  'parseGtccinfo: parseGtccinfo, parseGtact: parseGtact, gtactBandKind: gtactBandKind, ' +
  'gtactBandLabel: gtactBandLabel, UMTS_BAND_NAME: UMTS_BAND_NAME, ' +
  'GTACT_MODE: GTACT_MODE, PREF_ACT_NAME: PREF_ACT_NAME, BAND_PRESETS: BAND_PRESETS }; })()');
for (var k in ns) global[k] = ns[k];

var pass = 0, fail = 0;
function t(name, cond, detail) {
  if (cond) { pass++; }
  else { fail++; console.log('FAIL: ' + name + (detail ? ' —— ' + detail : '')); }
}
function eq(name, got, want) { t(name, got === want, 'got=' + JSON.stringify(got) + ' want=' + JSON.stringify(want)); }

/* ---------------- 1. GTCCINFO 布局 ---------------- */
// NR 服务小区：索引 10 = ss_sinr（实机行为：dB 原值）
var r = parseGtccinfo('+GTCCINFO: 1,9,460,0,149002,C2840C002,504990,128,,,16,69,69,64\n+GTCCINFO: 2,9,460,0,149003,C2840C003,504990,129,20,60,61,62\nOK');
t('NR 服务小区存在', r.serving && r.serving.serving === true);
eq('NR 服务小区 ss_rsrp(69)', r.serving.rsrp, -88);
eq('NR 服务小区 ss_rsrq(64)', r.serving.rsrq, -11.5);
eq('NR 服务小区 ss_sinr(16) 按 dB 原值', r.serving.sinr, 16);
t('NR 邻区存在', r.neighbors.length === 1);
eq('NR 邻区 ss_rsrp(61)', r.neighbors[0].rsrp, -96);
eq('NR 邻区 ss_rsrq(62)', r.neighbors[0].rsrq, -12.5);
eq('NR 邻区 ss_sinr(20)（手册索引 8）', r.neighbors[0].sinr, 20);

// LTE 邻区：f[8] 是 bandwidth，不得当频段；邻区不取 sinr
r = parseGtccinfo('+GTCCINFO: 2,4,460,0,1234,5678,1650,100,25,40,60,30\nOK');
eq('LTE 邻区 band 不取 bandwidth（由 ARFCN 推算）', r.neighbors[0].band, 'B3');
eq('LTE 邻区 rsrp(60)', r.neighbors[0].rsrp, -81);
eq('LTE 邻区 rsrq(30)', r.neighbors[0].rsrq, -5);
eq('LTE 邻区 sinr 为 null', r.neighbors[0].sinr, null);
eq('LTE 邻区 bandwidth 索引 8', r.neighbors[0].bandwidth, 25);

// LTE 服务小区：band=101→B1、rssnr 索引 10
r = parseGtccinfo('+GTCCINFO: 1,4,460,0,1234,5678,1650,100,101,25,30,60,60,30\nOK');
eq('LTE 服务小区 band 编码 101', r.serving.band, 'B1');
eq('LTE 服务小区 rsrp(60)', r.serving.rsrp, -81);
eq('LTE 服务小区 rsrq(30)', r.serving.rsrq, -5);
eq('LTE 服务小区 rssnr(30)', r.serving.sinr, 30);

// UMTS 服务小区：band=8 → UMTS 罗马数字表；rscp 索引 10；Ec/Io_lev 索引 14
r = parseGtccinfo('+GTCCINFO: 1,2,460,0,1234,5678,12730,100,8,30,60,8,64,0,40\nOK');
eq('UMTS 服务小区 band=8', r.serving.band, 'B8 (VIII)');
eq('UMTS 服务小区 rscp(60)', r.serving.rsrp, -61);
eq('UMTS 服务小区 Ec/Io_lev(40)', r.serving.rsrq, -4.5);
eq('UMTS 服务小区 sinr 为 null', r.serving.sinr, null);

// UMTS 邻区：rscp 在索引 14（15 字段布局）
r = parseGtccinfo('+GTCCINFO: 2,2,460,0,1234,5678,12731,101,5,7,8,9,45,50,60\nOK');
eq('UMTS 邻区 rscp(60)', r.neighbors[0].rsrp, -61);

// 前缀缺失的兼容形态
r = parseGtccinfo('1,9,460,0,1,2,504990,128,,,16,69,69,64\nOK');
t('无前缀形态可解析', r.serving && r.serving.rsrp === -88);

// layoutOk：字段数不足应被标记
r = parseGtccinfo('+GTCCINFO: 2,4,460,0,1234,5678,1650,100,25,40,60\nOK');
t('LTE 邻区字段数不足 layoutOk=false', r.neighbors[0].layoutOk === false);

/* ---------------- 2. GTACT 解析 ---------------- */
var g = parseGtact('+GTACT: 20,6,3,5078,101,102\nOK');
eq('GTACT rat=20', g.mode, 20);
eq('GTACT 优先制式 pref', g.pref.join(','), '6,3');
eq('GTACT 频段不含 PreferredAct', g.bands.join(','), '5078,101,102');
eq('GTACT prefNames', g.prefNames.join(','), 'NR 优先,LTE 优先');
g = parseGtact('+GTACT: 20,,,5041\nOK');
eq('GTACT 空位占位 pref 空', g.pref.length, 0);
eq('GTACT 空位占位频段', g.bands.join(','), '5041');
g = parseGtact('+GTACT: 10\nOK');
eq('GTACT rat=10 自动', g.mode, 10);

/* ---------------- 2b. GTACT 回显频段编码分类 ---------------- */
eq('gtactBandKind 1 → umts', gtactBandKind(1), 'umts');
eq('gtactBandKind 8 → umts', gtactBandKind(8), 'umts');
eq('gtactBandKind 10 → umts', gtactBandKind(10), 'umts');
eq('gtactBandKind 0 → auto', gtactBandKind(0), 'auto');
eq('gtactBandKind 101 → lte', gtactBandKind(101), 'lte');
eq('gtactBandKind 171 → lte', gtactBandKind(171), 'lte');
eq('gtactBandKind 5041 → nr', gtactBandKind(5041), 'nr');
eq('gtactBandKind 141 → lte（手册 GTACT <lte_band> 连续枚举 101..171）', gtactBandKind(141), 'lte');
eq('gtactBandLabel 3', gtactBandLabel(3), 'B3 (III)');
eq('gtactBandLabel 0', gtactBandLabel(0), '自动选频');
eq('gtactBandLabel 101', gtactBandLabel(101), 'B1');
eq('gtactBandLabel 5041', gtactBandLabel(5041), 'n41');
// 实机回显（CHANGELOG r17：`20,6,3,1,2,4,…`）：1,2,4 必须是 UMTS 而非 LTE
g = parseGtact('+GTACT: 20,6,3,1,2,4,101,102,5041\nOK');
eq('GTACT 回显 UMTS/LTE/NR 分类', g.bands.map(gtactBandKind).join(','),
  'umts,umts,umts,lte,lte,nr');

/* ---------------- 3. 阶梯索引边界 ---------------- */
eq('idx 0 = 低于 base 的下界', idxToLinear(0, -156, 1, 126), -157);
eq('idx 126 NR rsrp 最高档', rsrpFromIdx(126, 9), -31);
eq('idx 127 NR sinr 最高档', idxToLinear(127, -23, 0.5, 127), 40);
eq('idx 255 哨兵', rsrpFromIdx(255, 9), null);
eq('LTE rsrp maxIdx=97', rsrpFromIdx(97, 4), -44);
eq('WCDMA rscp maxIdx=96', rsrpFromIdx(96, 2), -25);
eq('WCDMA rscp 独立表 base', rsrpFromIdx(1, 2), -120);
eq('WCDMA Ec/Io_lev maxIdx=49', rsrqFromIdx(49, 2), 0);
eq('NR rsrq maxIdx=126', rsrqFromIdx(126, 9), 19.5);
eq('LTE rsrq maxIdx=34', rsrqFromIdx(34, 4), -3);
eq('sinr 255 无效', sinrFromRaw(255), null);
eq('sinr 物理区间外无效', sinrFromRaw(101), null);

/* ---------------- 4. 频段编码 ---------------- */
eq('NR 5041 → n41', bandName('5041', 9), 'n41');
eq('NR 5010 → n10', bandName('5010', 9), 'n10');
eq('NR 50512 → n512', bandName('50512', 9), 'n512');
eq('NR 141 在 NR 语境不得解 n41', bandName('141', 9), '编号 141');
eq('NR 504 不存在', bandName('504', 9), '编号 504');
eq('NR 5017 不存在', bandName('5017', 9), '编号 5017');
eq('LTE 101 → B1', bandName('101', 4), 'B1');
eq('LTE 171 → B71', bandName('171', 4), 'B71');
eq('LTE 188 越界', bandName('188', 4), '编号 188');
eq('LTE 116 手册连续枚举字面 B16', bandName('116', 4), 'B16');
eq('bandGroup 101 → lte', bandGroup(101), 'lte');
eq('bandGroup 5041 → nr', bandGroup(5041), 'nr');
eq('bandGroup 141（手册=B41）→ lte 而非旧版 nr', bandGroup(141), 'lte');

/* ---------------- 5. NR-ARFCN ---------------- */
eq('n78 ARFCN 633333 两段栅格', arfcnToBandLabel('633333', 9), 'n77/n78');
eq('n79 ARFCN 720000', arfcnToBandLabel('720000', 9), 'n79');
eq('n41 ARFCN 504990', arfcnToBandLabel('504990', 9), 'n41');
eq('n20/n28 重叠 795MHz', arfcnToBandLabel('159000', 9), 'n20/n28');
eq('n20 区间 811MHz', arfcnToBandLabel('162200', 9), 'n20');
eq('n28 独有区间 768MHz', arfcnToBandLabel('153600', 9), 'n28');
eq('LTE ARFCN 1650 → B3', arfcnToBandLabel('1650', 4), 'B3');

/* ---------------- 6. 前后端一致性对拍（Rust 等价实现内嵌） ---------------- */
// Rust band.rs::nr_band_no_is_valid 的等价转写（改动需与源码同步）
var FR1_ASSIGNED = [1,2,3,5,7,8,9,10,12,13,14,18,20,25,26,28,29,30,34,38,39,40,41,46,48,65,66,67,68,69,70,71,73,74,75,76,77,78,79,80,81,82,83,84,85,86,89,90,91,92,93,94,95,96,97,98,99,100,101,102,103,104,105];
function rustNrValid(n) {
  if (n >= 1 && n <= 105) return FR1_ASSIGNED.indexOf(n) >= 0;
  return (n >= 256 && n <= 269) || (n >= 510 && n <= 512) || (n >= 670 && n <= 710);
}
function rustNrFromCode(code) {
  var c = String(code).trim();
  if (c.indexOf('50') !== 0) return null;
  var rest = c.slice(2);
  if (!rest || !/^[0-9]+$/.test(rest)) return null;
  var n = parseInt(rest, 10);
  if (!isFinite(n) || n < 1 || !rustNrValid(n)) return null;
  return 'n' + n;
}
function rustLteFromCode(code) {
  var c = String(code).trim();
  if (!c || c.length > 6 || !/^[0-9]+$/.test(c)) return null;
  var v = parseInt(c, 10);
  if (v >= 101 && v <= 171) return 'B' + (v - 100);
  if (v >= 1 && v <= 88 && v !== 16) return 'B' + v;
  return null;
}
function rustNrArfcn(arfcn) {
  var mhz;
  if (arfcn <= 599999) mhz = arfcn * 0.005;
  else if (arfcn <= 2016666) mhz = 3000 + (arfcn - 600000) * 0.015;
  else return null;
  if (mhz >= 2110 && mhz <= 2170) return 'n1';
  if (mhz >= 1805 && mhz <= 1880) return 'n3';
  if (mhz >= 869 && mhz <= 894) return 'n5';
  if (mhz >= 925 && mhz <= 960) return 'n8';
  if (mhz >= 791 && mhz <= 803) return 'n20/n28';
  if (mhz >= 791 && mhz <= 821) return 'n20';
  if (mhz >= 758 && mhz <= 803) return 'n28';
  if (mhz >= 2010 && mhz <= 2025) return 'n34';
  if (mhz >= 2570 && mhz <= 2620) return 'n38/n41';
  if (mhz >= 1880 && mhz <= 1920) return 'n39';
  if (mhz >= 2300 && mhz <= 2400) return 'n40';
  if (mhz >= 2496 && mhz <= 2690) return 'n41';
  if (mhz >= 3300 && mhz <= 4200) return 'n77/n78';
  if (mhz >= 4400 && mhz <= 5000) return 'n79';
  return null;
}
var mismatches = 0;
for (var n = 1; n <= 105; n++) {
  var jsOk = nrBandNo(n);
  var rsOk = rustNrValid(n);
  if (jsOk !== rsOk) { mismatches++; console.log('MISMATCH FR1 n=' + n + ' js=' + jsOk + ' rust=' + rsOk); }
}
for (var c = 200; c <= 720; c++) {
  var code = '50' + c;
  var js = nrBandName(code);
  var rs = rustNrFromCode(code);
  var jsv = js === null ? null : 'n' + parseInt(js.slice(1), 10);
  if (jsv !== rs) { mismatches++; console.log('MISMATCH code=' + code + ' js=' + jsv + ' rust=' + rs); }
}
for (var v = 0; v <= 200; v++) {
  var jsL = lteBandName(v);
  var rsL = rustLteFromCode(String(v));
  if (jsL !== rsL) { mismatches++; console.log('MISMATCH lte v=' + v + ' js=' + jsL + ' rust=' + rsL); }
}
for (var a = 1000; a <= 2016000; a += 1000) {
  var jsA = arfcnToBandLabel(String(a), 9);
  var rsA = rustNrArfcn(a);
  if (jsA !== rsA) { mismatches++; console.log('MISMATCH arfcn=' + a + ' js=' + jsA + ' rust=' + rsA); }
}
t('前后端频段/ARFCN 对拍不一致数为 0', mismatches === 0, mismatches + ' 处不一致');

console.log('');
console.log('test_network.js: ' + pass + ' passed, ' + fail + ' failed');
process.exit(fail ? 1 : 0);
