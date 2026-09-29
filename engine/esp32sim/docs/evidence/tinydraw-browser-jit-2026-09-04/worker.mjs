import {runBattery} from './run.mjs';
runBattery(async name=>new Uint8Array(await (await fetch('/asset/'+name)).arrayBuffer()), e=>postMessage(e),new URL(import.meta.url).searchParams.get('jit')!=='0').catch(e=>postMessage({type:'error',line:e.stack||String(e)}));
