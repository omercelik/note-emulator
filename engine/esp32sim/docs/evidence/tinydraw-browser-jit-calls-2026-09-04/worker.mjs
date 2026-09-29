import {runBattery} from './run.mjs';
const params = new URL(import.meta.url).searchParams;
runBattery(async name=>new Uint8Array(await (await fetch('/asset/'+(name==='wasm'?(params.get('wasm')||'wasm'):name))).arrayBuffer()), e=>postMessage(e),params.get('jit')!=='0').catch(e=>postMessage({type:'error',line:e.stack||String(e)}));
