import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'; import { dirname, join } from 'node:path'
import { mkdtempSync, writeFileSync } from 'node:fs'; import { tmpdir } from 'node:os'; import { execSync } from 'node:child_process'
const here = dirname(fileURLToPath(import.meta.url))
const work = mkdtempSync(join(tmpdir(),'devdy-cxr-'))
writeFileSync(join(work,'README.md'),'# scratch\n')
try{execSync('git init -q && git add -A && git -c user.email=t@t.co -c user.name=t commit -qm init',{cwd:work})}catch{}

function phase(env, prompt, onReadyThread){
  return new Promise((resolve)=>{
    const ch=spawn('node',[join(here,'index.mjs')],{cwd:work,stdio:['pipe','pipe','pipe'],env:{...process.env,...env}})
    let tid=null,txt='',buf=''
    ch.stdout.on('data',c=>{buf+=c;let n;while((n=buf.indexOf('\n'))>=0){const l=buf.slice(0,n).trim();buf=buf.slice(n+1);if(!l)continue;let m;try{m=JSON.parse(l)}catch{continue}
      if(m.type==='_devdy_permission_request')ch.stdin.write(JSON.stringify({type:'permission_response',requestId:m.requestId,decision:'allow'})+'\n')
      else if(m.type==='system'){tid=m.session_id; if(onReadyThread)onReadyThread()}
      else if(m.type==='assistant')for(const b of m.message?.content??[])if(b.type==='text')txt+=b.text+' '
    }})
    ch.stderr.on('data',d=>process.stderr.write('[e]'+d))
    ch.on('exit',()=>resolve({tid,txt}))
    ch.stdin.write(JSON.stringify({type:'prompt',text:prompt})+'\n')
    setTimeout(()=>{try{ch.kill()}catch{}},90000)
  })
}

console.log('phase 1: establish a fact')
const p1=await phase({DEVDY_PERMISSION_MODE:'plan'}, 'Remember this codeword: BANANA-42. Just reply OK, do not run any commands.')
console.log('  threadId:',p1.tid,'| said:',p1.txt.slice(0,60))
console.log('phase 2: resume and recall')
const p2=await phase({DEVDY_PERMISSION_MODE:'plan',DEVDY_RESUME_SESSION:p1.tid}, 'What was the codeword I told you earlier? Reply with just the codeword.')
console.log('  said:',p2.txt.slice(0,80))
const ok = p1.tid && p2.txt.includes('BANANA-42')
console.log('VERDICT:', ok?'PASS ✅ (resume preserved context)':'FAIL ❌')
process.exit(ok?0:1)
