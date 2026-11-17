"use strict";

const $ = (selector) => document.querySelector(selector);
const input = $("#text-input");
const form = $("#detector-form");
const analyzeButton = $("#analyze");
const maxLength = Number(input.dataset.maxLength);
const labels = {
  EMAIL: "邮箱地址",
  PHONE: "手机号码",
  ID: "证件号码",
  ORG: "组织",
  DATE: "日期与时间",
  CONFIDENTIAL: "保密内容",
  EMAIL_ADDRESS: "邮箱地址",
  PHONE_NUMBER: "手机号码",
  PASSWORD: "密码",
  CHINA_ID: "身份证号",
  CREDIT_CARD: "银行卡号",
  PERSON: "人名",
  LOCATION: "地点",
  ORGANIZATION: "组织",
  MEDICAL_INFO: "医疗信息",
  ADDRESS: "家庭住址",
  SECRET: "密钥与凭据",
  MONEY: "金额",
  DATE_TIME: "日期与时间",
  NUMBER: "数字",
  LICENSE_PLATE: "车牌号",
  IP_ADDRESS: "IP 地址",
  MAC_ADDRESS: "MAC 地址",
  ACCOUNT: "账号",
  ID_NUMBER: "证件号码",
};
const sourceLabels = { regex: "正则", rule: "规则", ner: "NER" };
const samples = {
  privacy: {
    text: '姓名：张三；公司：示例科技公司\n邮箱：lin@example.com，电话：+86 138-0013-8000\n工资：25000，奖金：USD 1,200.50\n日期：2026-10-26T09:30:00+08:00\n地址：北京市朝阳区示例路88号\n诊断：示例病历，仅用于测试\napi_key="sk-demo_1234567890"\nAuthorization: Bearer demo-access-token\n密码："two words!"；重复密码：two words!\n设备：192.0.2.10，编号：42',
  },
  contact: {
    text: "你好，这是我的联系方式：\n邮箱 lin@example.com，手机号 13800138000。\n登录密码：Cloud!2026\n我的身份证号是 11010519491231002X。",
  },
  medical: {
    text: "我被诊断患有疾病，正在接受治疗。\n我的家庭住址是某市某街道。\n请将这些信息保密。",
  },
  english: {
    text: "Please contact me at alex@example.com.\nPassword: Sample!2026\nMy credit card: 4111 1111 1111 1111.",
  },
  handover: {
    text: `【项目交接邮件｜以下人物、机构与凭据均为虚构测试数据】

嘉宁，你好：
星河数据实验室在北京的试点即将进入联合验收。林知远负责接口联调，陈嘉宁负责数据复核；上海团队只接收遮蔽后的材料。请保留任务背景和处理意见，发送前检查正文、转发记录及日志附件中可能夹带的个人信息。

客户联系人在回信中留下邮箱 lin.zhiyuan@example.com，说明工作日可拨打 +86 138-0013-8000，无法接通时再联系座机 010-12345678。快递登记的收货地址：北京市朝阳区示例路88号测试中心；备注要求前台签收，不要将联系电话写在共享看板上。历史表单中的姓名：林知远；这与正文提到的林知远是同一个测试联系人，需要检查重复出现时是否都被遮蔽。

关于费用，供应商将本轮报价调整为 USD 12,480.50，计划在 2026-11-03T09:30:00+08:00 开始验收。测试付款使用银行卡：4111 1111 1111 1111；该号码仅用于沙箱演示。请把“报价仍待确认”“需完成回归后再付款”等正常意见保留在邮件中，避免整封邮件都失去阅读价值。

附件中误粘了一段演示配置：DB_PASSWORD="two words!"；重复密码：two words!；api_key="sk-demo_handover_1234567890"。这不是生产凭据，但发布前仍应完整遮蔽，尤其不能只遮住带空格密码的第一个单词。另一处工单再次提到 two words!，也不应该因为缺少字段标签而留在遮蔽文本中。

联调日志写着客户端从 192.0.2.10 访问测试服务，后续切换到 IPv6 地址 2001:db8::10，设备标识为 00:1A:2B:3C:4D:5E。请求头片段 Authorization: Bearer demo-handover-access-token 被复制到了回复中，账号：demo-handover；护照号：E12345678。这些片段和叙述性的人名、机构、地点混在一起，请分别观察正则、规则和 NER 的命中来源。

最后，请在确认缺陷关闭后回复验收结论。公开版本应能说明我们做了什么、下一步由谁跟进，同时不暴露联系信息、内部凭据和测试身份资料。`,
  },
  casefile: {
    text: `【就诊随访与保险沟通记录｜全部内容为虚构测试材料】

这份记录由示例健康管理中心整理，用于演示长篇医疗叙述与一般工作安排混合时的隐私检测。周雨桐最近从杭州搬到上海，准备把检查资料转交给新的随访团队。原文中既有可以公开的预约流程，也有只应由经办人员查看的信息，不能简单依靠“这是一份病历”就判断每一句话都属于隐私。

患者姓名：周雨桐；回访邮箱：yutong.zhou@example.com；联系电话：+86 139-0013-9000。周雨桐在回信中写道：“我被诊断患有疾病，正在接受治疗。我希望随访时尽量使用邮件，工作时间不方便接听电话。”她又补充自己近期经常夜间醒来，正在记录用药后的感受。这些自然表达没有始终使用固定的表单格式，可以检查规则与实体识别的覆盖范围。

转诊表中另有两行原始内容：
诊断：虚构的慢性病随访记录，仅用于测试，不构成医疗建议。
用药：示例药物A，每日一次；检查结果：虚构指标待复查。
经办人员的处理意见是先核对材料是否齐全，再安排下一次沟通。这里的流程说明应与患者的诊断、用药和检查信息一并阅读，但并不具有相同的隐私性质。

她在另一封信中说明：“我的家庭住址是某市某街道。”邮寄单随后补充联系地址：上海市示例区春和路16号测试公寓；请寄送纸质回执，不要向公开群聊转发门牌信息。身份证号：11010519491231002X；保险账户：demo-insurance-2026；这些是合成资料中的测试字段，需要检查它们与普通叙述是否有不同的检测范围。

预约安排在 2026-11-06 14:30，垫付费用为 1,280.50元。经办人员提醒她把报销说明与原始凭证分开归档，公开会议纪要只保留“材料已补齐，等待审核”的状态。此前系统迁移时，演示用的密码："quiet river dawn!"；重复密码：quiet river dawn!；也被误带入邮件，发送前应完整移除这类内容。

随访结语中再次出现周雨桐和 yutong.zhou@example.com，供测试同文重复值的遮蔽。中心计划先确认接收方的权限，再发送处理后的摘要；未涉及个人情况的预约规则与材料清单可以继续保留，方便下一位经办人员理解来龙去脉。`,
  },
  english_report: {
    text: `Subject: Support escalation and account migration review
All people, organizations, addresses and credentials below are fictional test data.

Hello Maya,

The team at Northstar Research is reviewing a support escalation before sending the case to our London partner. Alice Morgan spoke with Daniel Chen in Singapore, and both agreed that the public summary should explain the incident without exposing the customer's contact details or the configuration copied into the original ticket.

The customer asked us to reply to alice.morgan@example.com and left +1 202 555 0123 as a callback number. The shipping record says address: "123 Example Street, London"; the delivery instructions ask the receptionist to hold the package. A legacy record contains passport: E12345678; account: demo-northstar-2026. These fields are mixed with ordinary prose so that we can compare explicit rules with the names, places and organizations returned by NER.

The finance note estimates the adjustment at USD 2,450.75 and proposes a review on 2026-11-09T10:15:00Z. The sandbox payment record contains credit card: 4111 1111 1111 1111; this is a test card and must still be removed from a shared copy. The sentence explaining why the adjustment needs approval should remain understandable after masking.

During the handover, someone pasted password: "paper crane sunrise!"; repeated password: paper crane sunrise!; into the discussion. A later reply repeated paper crane sunrise! without a field label. The attached configuration also contains API_KEY="sk-demo_northstar_1234567890" and the request header Authorization: Bearer demo-northstar-access-token; neither belongs in the external report.

The technical timeline mentions 192.0.2.25, 2001:db8::25 and the device 00:1A:2B:3C:4D:5E. A connection string was copied as postgres://demo_user:demo_pass@example.test/support. We should retain the sequence of events and the proposed mitigation while removing credentials embedded in logs, URLs and free-form notes.

Please send the cleaned version to Alice Morgan through the approved channel. The closing paragraph repeats alice.morgan@example.com to check whether known values are masked consistently, even when the second occurrence has no label. Once the review is complete, the team can publish the operational lessons without including the private material from the original thread.`,
  },
};
let revision = 0;
let activeController = null;
let loading = false;
let lastResults = null;
let historyMode = false;
function announce(text) {
  $("#live-message").textContent = text;
}
function selectedEngines() {
  return [...document.querySelectorAll('input[name="engines"]:checked')].map(
    (item) => item.value,
  );
}
function updateControls() {
  const length = Array.from(input.value).length;
  $("#char-count").textContent = `${length} / ${maxLength}`;
  $("#char-count").classList.toggle("over-limit", length > maxLength);
  const threshold = $("#mask-threshold").valueAsNumber;
  analyzeButton.disabled = historyMode || loading || !input.value.trim() || length > maxLength ||
    !Number.isFinite(threshold) || threshold < 0 || threshold > 1;
  $("#analyze-label").textContent = loading ? "检测中…" : "开始检测";
  $("#active-pipeline").textContent = historyMode ? "当次网关策略 · 记录只读" : selectedEngines().includes("ner")
    ? "正则 + 精确规则 + NER" : "正则 + 精确规则";
}
function resetResults() {
  lastResults = null;
  $("#result-content").hidden = true;
  $("#result-count").hidden = true;
  $("#empty-state").hidden = false;
  $("#loading-state").hidden = true;
  $("#empty-title").textContent = "每一处敏感，都有迹可循";
  $("#empty-description").textContent =
    "输入文本并开始检测，结果将在这里呈现。";
  $("#result-summary").textContent = "等待检测";
  $("#highlighted-text").replaceChildren();
  $("#findings").replaceChildren();
  $("#masked-panel").hidden = true;
  $("#masked-text").value = "";
  $("#masked-summary").textContent = "";
  $("#error-message").hidden = true;
  $(".results-card").setAttribute("aria-busy", "false");
}
function invalidate() {
  revision += 1;
  if (activeController) activeController.abort();
  loading = false;
  resetResults();
  updateControls();
}
function node(tag, className, text) {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
}
function renderHighlights(text, results) {
  // API offsets count Unicode code points; JavaScript's string.slice counts UTF-16 units.
  const characters = Array.from(text);
  const boundaries = [
    ...new Set([
      0,
      characters.length,
      ...results.flatMap((r) => [r.start, r.end]),
    ]),
  ].sort((a, b) => a - b);
  const fragment = document.createDocumentFragment();
  for (let index = 0; index < boundaries.length - 1; index += 1) {
    const start = boundaries[index],
      end = boundaries[index + 1];
    const covering = results
      .map((result, i) => ({ result, i }))
      .filter(({ result }) => result.start <= start && result.end >= end);
    const part = characters.slice(start, end).join("");
    if (covering.length) {
      const mark = node("mark", "", part);
      mark.dataset.results = covering.map(({ i }) => i).join(" ");
      mark.title = [
        ...new Set(
          covering.map(
            ({ result }) => labels[result.entity_type] || result.entity_type,
          ),
        ),
      ].join("、");
      fragment.append(mark);
    } else fragment.append(document.createTextNode(part));
  }
  $("#highlighted-text").replaceChildren(fragment);
}
function focusResult(index, button) {
  document
    .querySelectorAll(".finding")
    .forEach((item) => item.classList.toggle("selected", item === button));
  let first;
  document.querySelectorAll("#highlighted-text mark").forEach((mark) => {
    const active = mark.dataset.results.split(" ").includes(String(index));
    mark.classList.toggle("active", active);
    if (active && !first) first = mark;
  });
  if (first) first.scrollIntoView({ block: "nearest", behavior: "smooth" });
}
function renderResults(data, text) {
  lastResults = data;
  $("#result-count").hidden = false;
  $("#result-count").textContent = String(data.count);
  $("#result-summary").textContent =
    data.recorded ? `${data.category_count} 类线索 · 当次网关记录` : `${data.category_count} 类线索 · ${data.elapsed_ms} ms`;
  $("#loading-state").hidden = true;
  $("#empty-state").hidden = data.count > 0;
  $("#result-content").hidden = data.count === 0;
  $("#masked-text").value = data.masked_text;
  $("#masked-panel").hidden = false;
  $("#masked-summary").textContent =
    data.recorded ? `当次网关实际发送的文本 · 已遮蔽 ${data.masked_count} 个范围。`
      : `已遮蔽 ${data.masked_count} 个范围 · 分数阈值 ≥ ${data.mask_threshold.toFixed(2)}。低分候选保留供复核。`;
  if (!data.count) {
    $("#empty-title").textContent = "暂未发现敏感信息";
    $("#empty-description").textContent =
      "当前设置下没有命中结果，发送前仍建议人工复核。";
  } else {
    renderHighlights(text, data.results);
    const characters = Array.from(text);
    const fragment = document.createDocumentFragment();
    data.results.forEach((result, index) => {
      const button = node("button", "finding");
      button.type = "button";
      const top = node("span", "finding-top");
      top.append(
        node(
          "span",
          "entity-label",
          labels[result.entity_type] || result.entity_type,
        ),
        node("span", "score", Number.isFinite(result.score) ? `分数 ${result.score.toFixed(2)}` : "策略命中"),
      );
      const snippet = node(
        "span",
        "finding-snippet",
        characters.slice(result.start, result.end).join(""),
      );
      const meta = node("span", "finding-meta");
      const method =
        result.explanation === "Semantic rule similarity"
          ? "语义匹配"
          : result.sources
              .map((source) => sourceLabels[source] || source)
              .join(" + ");
      meta.append(
        node("span", "finding-sources", method),
        node("span", "", `位置 ${result.start}–${result.end}`),
        node(
          "span",
          "",
          data.recorded ? "已由网关遮蔽" : result.mask_eligible ? "已达遮蔽阈值" : "低于阈值 · 未遮蔽",
        ),
        ...(result.validation === "failed"
          ? [node("span", "", "校验未通过")]
          : []),
      );
      button.append(top, snippet, meta);
      button.addEventListener("click", () => focusResult(index, button));
      fragment.append(button);
    });
    $("#findings").replaceChildren(fragment);
  }
  announce(
    `检测完成，发现 ${data.count} 处、${data.category_count} 类敏感线索。`,
  );
}

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  if (analyzeButton.disabled) return;
  resetResults();
  loading = true;
  const current = ++revision;
  const text = input.value;
  activeController = new AbortController();
  $("#empty-state").hidden = true;
  $("#loading-state").hidden = false;
  $("#result-summary").textContent = "正在检测";
  $(".results-card").setAttribute("aria-busy", "true");
  updateControls();
  announce("正在检测文本。首次使用模型可能需要较长时间。");
  try {
    const response = await fetch("/api/analyze", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        text,
        use_ner: selectedEngines().includes("ner"),
        mask_threshold: $("#mask-threshold").valueAsNumber,
        mask_style: $("#mask-style").value,
      }),
      signal: activeController.signal,
    });
    const data = await response.json();
    if (current !== revision) return;
    if (!response.ok) throw new Error(data.error || "检测未完成，请重试。");
    renderResults(data, text);
  } catch (error) {
    if (current !== revision || error.name === "AbortError") return;
    resetResults();
    $("#error-message").hidden = false;
    $("#error-message").textContent =
      error instanceof TypeError
        ? "无法连接检测服务，请确认服务已启动后重试。"
        : error.message;
    $("#result-summary").textContent = "检测未完成";
    announce("检测未完成。");
  } finally {
    if (current === revision) {
      loading = false;
      activeController = null;
      $(".results-card").setAttribute("aria-busy", "false");
      updateControls();
    }
  }
});

input.addEventListener("input", invalidate);
$("#mask-threshold").addEventListener("input", invalidate);
form.addEventListener("change", invalidate);
input.addEventListener("keydown", (event) => {
  if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
    event.preventDefault();
    if (!analyzeButton.disabled) form.requestSubmit();
  }
});
$("#clear").addEventListener("click", () => {
  input.value = "";
  invalidate();
  input.focus();
  announce("文本和结果已清空。");
});
document.querySelectorAll("[data-sample]").forEach((button) => {
  button.addEventListener("click", () => {
    const sample = samples[button.dataset.sample];
    input.value = sample.text;
    invalidate();
    announce("已载入示例文本。");
  });
});
$("#copy-results").addEventListener("click", async () => {
  if (!lastResults) return;
  try {
    await navigator.clipboard.writeText(JSON.stringify(lastResults, null, 2));
    announce("结果已复制。");
  } catch {
    announce("浏览器未允许复制，请手动选择结果。");
  }
});
$("#copy-masked").addEventListener("click", async () => {
  if (!lastResults) return;
  try {
    await navigator.clipboard.writeText(lastResults.masked_text);
    announce("遮蔽文本已复制。");
  } catch {
    $("#masked-text").focus();
    $("#masked-text").select();
    announce("已选中遮蔽文本，请使用系统复制快捷键。");
  }
});
updateControls();

const statusLabels = {pending:"检测中",streaming:"回复中",completed:"已完成",error:"未完成"};
const roleLabels = {system:"系统",developer:"系统",user:"用户",assistant:"助手",tool:"工具结果"};
let selectedSession = null;
let selectedRecord = null;
let sessionVersion = "";
let timelineVersion = "";
let polling = false;
let clearingConversations = false;
let conversationRevision = 0;

function resetConversationDetails() {
  selectedRecord = null;
  setHistoryMode(false);
  input.value = "";
  invalidate();
  $("#detail-context").textContent = "选择消息查看检测与遮蔽详情";
}

$("#clear-conversations").addEventListener("click", async () => {
  if (clearingConversations || !window.confirm(
    "清空所有已保存的 Agent 对话和检测详情？此操作无法撤销。\n\n清空后请新开 Agent 会话开始测试；正在使用的客户端仍保留原来的对话上下文。",
  )) return;
  clearingConversations = true;
  conversationRevision += 1;
  const button = $("#clear-conversations");
  button.disabled = true;
  button.textContent = "正在清空…";
  try {
    const response = await fetch("/api/conversations", {
      method: "DELETE",
      headers: {"Content-Type":"application/json"},
      body: JSON.stringify({confirm:true}),
    });
    const data = await response.json();
    if (!response.ok) throw new Error(data.error || "清空未完成，请重试。");
    selectedSession = null;
    sessionVersion = "";
    timelineVersion = "";
    resetConversationDetails();
    $("#session-list").replaceChildren(node("p", "conversation-hint", "尚无会话"));
    renderTimeline([]);
    const message = `已清空 ${data.cleared_sessions} 个会话。请新开 Agent 会话开始测试。`;
    $("#conversation-notice").textContent = message;
    $("#conversation-notice").hidden = false;
    announce(message);
  } catch (error) {
    const message = error instanceof TypeError ? "无法连接会话服务，清空未完成。" : error.message;
    $("#conversation-notice").textContent = message;
    $("#conversation-notice").hidden = false;
    announce(message);
  } finally {
    clearingConversations = false;
    button.textContent = "清空记录";
    button.disabled = false;
  }
  syncConversations();
});

function setHistoryMode(enabled) {
  historyMode = enabled;
  input.readOnly = enabled;
  for (const element of document.querySelectorAll('#clear, [data-sample], #mask-style, #mask-threshold, input[value="ner"]')) {
    element.disabled = enabled;
  }
  $("#manual-mode").hidden = !enabled;
  $("#editor-hint").textContent = enabled ? "当次发送前的原始文本 · 只读" : "仅在点击检测后分析";
  $(".settings-sidebar").hidden = enabled;
  form.classList.toggle("record-detail", enabled);
  $("#input-title").lastChild.textContent = enabled ? " 发送前的原文" : " 待检测文本";
  updateControls();
}

function showSnapshot(snapshot, context, key) {
  invalidate();
  setHistoryMode(true);
  selectedRecord = key;
  input.value = snapshot.text;
  updateControls();
  renderResults(snapshot.analysis, snapshot.text);
  $("#detail-context").textContent = context;
  document.querySelectorAll('[data-record-key]').forEach(button => {
    button.classList.toggle("selected", button.dataset.recordKey === key);
    button.setAttribute("aria-pressed", String(button.dataset.recordKey === key));
  });
}

$("#manual-mode").addEventListener("click", () => {
  selectedRecord = null;
  setHistoryMode(false);
  invalidate();
  $("#detail-context").textContent = "独立文本检测";
  document.querySelectorAll('[data-record-key]').forEach(button => {
    button.classList.remove("selected"); button.setAttribute("aria-pressed", "false");
  });
});

function messageText(message) {
  const content = typeof message.content === "string" ? message.content
    : Array.isArray(message.content) ? message.content.map(part => part.text || "").join("\n") : "";
  const tools = message.tool_calls?.length ? "\n" + message.tool_calls.map(call =>
    `${call.function?.name || "工具调用"}(${call.function?.arguments || ""})`).join("\n") : "";
  return content + tools || "（空消息）";
}

function agentLabel(agent) {
  if (!agent || agent === "xiaoo") return "xiaoO";
  if (agent === "deepseek-harness") return "DeepSeek Harness";
  return agent;
}

function renderTimeline(exchanges) {
  const fragment = document.createDocumentFragment();
  let previousMessages = [];
  let previousResponse = null;
  exchanges.forEach((exchange, turn) => {
    const label = agentLabel(exchange.agent);
    const roleLabel = role => role === "assistant" ? label : roleLabels[role] || role;
    const group = node("section", "exchange");
    group.append(node("p", "exchange-meta", `第 ${turn + 1} 次模型调用 · ${new Date(exchange.created_at).toLocaleTimeString()} · ${statusLabels[exchange.status] || exchange.status}`));
    const messages = exchange.messages || [];
    let shared = 0;
    while (shared < previousMessages.length && shared < messages.length &&
      JSON.stringify(previousMessages[shared]) === JSON.stringify(messages[shared])) shared += 1;
    // The prior response is already visible below its request. Keep its exact
    // outbound version inspectable when the Agent sends it back in the next round.
    messages.forEach((message, index) => {
      if (index < shared) return;
      const holder = node("div", `chat-message role-${message.role || "unknown"}`);
      holder.append(node("strong", "message-role", roleLabel(message.role)));
      if (message.name || message.tool_call_id) holder.append(node("span", "subtle", ` ${message.name || message.tool_call_id}`));
      const snapshots = exchange.snapshots.filter(snapshot => snapshot.path.startsWith(`/messages/${index}/`));
      holder.append(node("pre", "message-content", messageText(message)));
      if (previousResponse && index === shared && message.role === "assistant") {
        holder.append(node("small", "subtle", "上一轮回复再次发送给模型"));
      }
      if (snapshots.length) {
        snapshots.forEach((snapshot, part) => {
          const key = `${exchange.id}:${snapshot.path}`;
          const button = node("button", "record-button", snapshots.length > 1 ? `查看片段 ${part + 1} 的检测与遮蔽` : "查看检测与遮蔽");
          button.type = "button";
          button.dataset.recordKey = key;
          button.classList.toggle("selected", selectedRecord === key);
          button.setAttribute("aria-pressed", String(selectedRecord === key));
          button.addEventListener("click", () => {
            showSnapshot(snapshot, `第 ${turn + 1} 次调用 · ${roleLabel(message.role)} · ${snapshot.path} · 实际发送记录`, key);
            $("#detail-context").scrollIntoView({block:"start",behavior:"smooth"});
          });
          holder.append(button);
        });
      } else holder.append(node("small", "subtle", exchange.status === "error" ? "请求未通过网关，无已发送的遮蔽结果。" : "等待网关检测记录…"));
      if (["system","developer"].includes(message.role)) {
        const details = node("details", "system-message");
        details.append(node("summary", "", "系统提示词"), holder); group.append(details);
      } else group.append(holder);
    });
    if (exchange.response && (exchange.response.content || exchange.response.tool_calls?.length)) {
      const reply = node("div", "chat-message role-assistant");
      reply.append(node("strong", "message-role", `${label} 回复`), node("pre", "message-content", messageText(exchange.response)),
        node("small", "subtle", "本地恢复后的回复；工具调用与工具结果按执行顺序记录。"));
      group.append(reply);
    }
    if (exchange.error) group.append(node("p", "conversation-error", `本次调用未完成：${exchange.error}`));
    fragment.append(group);
    previousMessages = messages;
    previousResponse = exchange.response;
  });
  if (!exchanges.length) fragment.append(node("p", "conversation-hint", "等待第一条对话记录…"));
  $("#conversation-timeline").replaceChildren(fragment);
}

async function syncConversations() {
  if (polling || clearingConversations || document.hidden) return;
  polling = true;
  const currentRevision = conversationRevision;
  try {
    const response = await fetch("/api/conversations", {cache:"no-store"});
    if (!response.ok) throw new Error("会话服务暂时不可用，正在重连…");
    const data = await response.json();
    if (currentRevision !== conversationRevision) return;
    $("#conversation-status").textContent = data.enabled ? "自动同步 · 本机保存" : "尚未连接 · 使用 cpctl start 启用";
    $("#clear-conversations").disabled = !data.enabled || !data.sessions.length;
    if (!data.enabled) return;
    if (!selectedSession && data.sessions.length) selectedSession = data.sessions[0].id;
    if (selectedSession && !data.sessions.some(session => session.id === selectedSession)) {
      selectedSession = data.sessions[0]?.id || null; timelineVersion = "";
      if (historyMode) resetConversationDetails();
    }
    const version = JSON.stringify([selectedSession, data.sessions]);
    if (version !== sessionVersion) {
      const fragment = document.createDocumentFragment();
      for (const session of data.sessions) {
        const button = node("button", "session-button"); button.type = "button";
        button.classList.toggle("selected", session.id === selectedSession);
        button.setAttribute("aria-pressed", String(session.id === selectedSession));
        button.append(node("strong", "", session.title), node("small", "", `${agentLabel(session.agent)} · ${new Date(session.created_at).toLocaleString()} · ${session.exchange_count} 次调用 · ${statusLabels[session.status] || session.status}`));
        button.addEventListener("click", () => {
          selectedSession = session.id; timelineVersion = ""; sessionVersion = "";
          selectedRecord = null;
          if (historyMode) {setHistoryMode(false);input.value="";invalidate();$("#detail-context").textContent = "选择消息查看检测与遮蔽详情";}
          syncConversations();
        });
        fragment.append(button);
      }
      if (!data.sessions.length) fragment.append(node("p", "conversation-hint", "尚无会话"));
      $("#session-list").replaceChildren(fragment); sessionVersion = version;
    }
    if (selectedSession) {
      const currentSession = selectedSession;
      const detail = await fetch(`/api/conversations/${currentSession}`, {cache:"no-store"});
      if (!detail.ok) throw new Error("会话读取失败，正在重试…");
      const records = await detail.json();
      if (currentRevision !== conversationRevision || currentSession !== selectedSession) return;
      const nextVersion = JSON.stringify(records);
      if (nextVersion !== timelineVersion) {renderTimeline(records.exchanges);timelineVersion = nextVersion;}
    } else {renderTimeline([]);timelineVersion = "";}
  } catch (error) {
    if (currentRevision !== conversationRevision) return;
    $("#conversation-status").textContent = error.message || "连接中断，正在重连…";
  } finally {polling = false;}
}
syncConversations();
setInterval(syncConversations, 1500);
document.addEventListener("visibilitychange", syncConversations);
