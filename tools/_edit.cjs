// 用 node 做精确替换，避开 PowerShell 的编码问题
const fs=require("node:fs");
const f="crates/builtin/src/lib.rs";
let t=fs.readFileSync(f,"utf8");
function p(find,rep,all){ if(!t.includes(find)){console.error("未找到: "+find.slice(0,60));process.exit(2);} t = all? t.split(find).join(rep) : t.replace(find,rep); }

// 两处 SessionState 初始化
p("                    step_seq: parent.step_seq,\n                    tool_last_step: parent.tool_last_step.clone(),\n                };",
  "                    step_seq: parent.step_seq,\n                    tool_last_step: parent.tool_last_step.clone(),\n                    pending_steer: Default::default(),\n                };");
p("            step_seq: 0,\n            tool_last_step: std::collections::HashMap::new(),\n        };",
  "            step_seq: 0,\n            tool_last_step: std::collections::HashMap::new(),\n            pending_steer: Default::default(),\n        };");
fs.writeFileSync(f,t,"utf8");
console.log("ok");
