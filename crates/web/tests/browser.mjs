// Runs the real WASM app against browser_fixture's isolated protocol stream.
// Build browser_fixture and the web app first. Set WEB_ASSETS to wasm-bindgen's
// output, PLAYWRIGHT_MODULE to an installed Playwright ESM module, and optionally
// CHROMIUM_EXECUTABLE. No daemon credentials or model requests are used.
import { createServer } from 'node:http';
import { readFile, mkdir } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import path from 'node:path';
import assert from 'node:assert/strict';
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const output = process.env.WEB_SHOTS || 'target/web-browser-checks';
await mkdir(output, { recursive: true });
const server = createServer(async (req, res) => {
  res.setHeader('Content-Security-Policy', "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' data:; connect-src 'self'; manifest-src 'self'; worker-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'");
  if (req.url === '/session') { res.setHeader('Content-Type','application/json'); return res.end(JSON.stringify({state:'paired',notifications:true})); }
  const name = req.url === '/' ? 'index.html' : path.basename(req.url.split('?')[0]);
  const dir = ['app.js','app_bg.wasm'].includes(name) ? process.env.WEB_ASSETS : 'crates/web/static';
  try {
    const data = await readFile(path.join(dir,name));
    res.setHeader('Content-Type', name.endsWith('.wasm') ? 'application/wasm' : name.endsWith('.js') ? 'text/javascript' : name.endsWith('.css') ? 'text/css' : name.endsWith('.png') ? 'image/png' : 'text/html');
    res.end(data);
  } catch { res.writeHead(404); res.end(); }
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));
const base = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch({ headless:true, executablePath:process.env.CHROMIUM_EXECUTABLE });
const errors=[];
const actions=[];
const sockets=[];
try {
  const context=await browser.newContext({viewport:{width:390,height:844},isMobile:true,hasTouch:true});
  await context.routeWebSocket('**/ws', ws=>{
    sockets.push(ws);
    const fixture=spawn('target/debug/examples/browser_fixture',[],{stdio:['pipe','pipe','pipe']});
    createInterface({input:fixture.stdout}).on('line',line=>ws.send(Buffer.from(line,'hex')));
    createInterface({input:fixture.stderr}).on('line',line=>actions.push(line));
    ws.onMessage(message=>fixture.stdin.write(Buffer.from(message).toString('hex')+'\n'));
    ws.onClose(()=>fixture.kill());
  });
  const page=await context.newPage();
  page.on('pageerror',e=>errors.push(e.message));
  await page.goto(base);
  await page.locator('.home .chat .title').filter({hasText:'Polish the mobile experience'}).waitFor();
  const noOverflow=async()=>assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),'horizontal page overflow');
  await noOverflow();
  await page.screenshot({path:`${output}/phone-home.png`,fullPage:true});
  await page.getByRole('button',{name:/Needs you/}).click();
  await page.locator('.home .chat .title').filter({hasText:'Polish the mobile experience'}).waitFor({state:'hidden'});
  await page.getByLabel('Search agents and workspaces').fill('authentication');
  await page.getByRole('button',{name:/Fix the deployment/}).waitFor({state:'hidden'});
  await page.getByLabel('Search agents and workspaces').fill('');
  await page.getByRole('button',{name:/Agents/}).click();
  await page.locator('.home .chat .title').filter({hasText:'Polish the mobile experience'}).click();
  await page.locator('#message').waitFor();
  await page.getByText('Up next · 1',{exact:true}).waitFor();
  assert.deepEqual(await page.locator('canvas.image-preview').first().evaluate(c=>Array.from(c.getContext('2d').getImageData(0,0,1,1).data)),[35,90,65,255]);
  await noOverflow();
  await page.screenshot({path:`${output}/phone-chat.png`});
  assert.equal(await page.locator('.turn').count(),8,'opening should only render the recent page');
  assert.equal(await page.locator('.turn').first().getAttribute('data-turn'),'32');
  const tightComposer=async()=>{
    await page.waitForFunction(()=>{
      const bottom=document.querySelector('.composer').getBoundingClientRect().bottom;
      const gap=visualViewport.offsetTop+visualViewport.height-bottom;
      return gap>=-1 && gap<=7;
    });
    const gap=await page.locator('.composer').evaluate(e=>visualViewport.offsetTop+visualViewport.height-e.getBoundingClientRect().bottom);
    assert(gap>=-1 && gap<=7,`composer bottom gap ${gap}px`);
  };
  await tightComposer();

  // The real file input, local decode/PNG normalization, thumbnails and wire
  // submission. No provider credentials or model requests are involved.
  const photo=await page.evaluate(()=>{
    const c=document.createElement('canvas');c.width=20;c.height=30;
    const ctx=c.getContext('2d');ctx.fillStyle='#ef3344';ctx.fillRect(0,0,20,30);
    return {png:c.toDataURL('image/png').split(',')[1],jpeg:c.toDataURL('image/jpeg').split(',')[1]};
  });
  const png={name:'screenshot.png',mimeType:'image/png',buffer:Buffer.from(photo.png,'base64')};
  const jpeg={name:'photo.jpg',mimeType:'image/jpeg',buffer:Buffer.from(photo.jpeg,'base64')};
  const chooser=page.waitForEvent('filechooser');
  await page.getByRole('button',{name:'Attach photos',exact:true}).click();
  await (await chooser).setFiles([png,jpeg]);
  await page.waitForFunction(()=>document.querySelectorAll('.draft-photo').length===2 && !document.querySelector('#photo-input').disabled);
  assert.equal(await page.locator('.draft-photo canvas').first().evaluate(c=>c.height),30);
  assert(await page.getByRole('button',{name:'Stop & continue',exact:true}).isVisible(),'image steering label should explain interruption');
  await noOverflow();
  await page.screenshot({path:`${output}/phone-photo-draft.png`});
  await page.getByRole('button',{name:'Remove photo 1',exact:true}).click();
  await page.waitForFunction(()=>document.querySelectorAll('.draft-photo').length===1);
  await page.getByRole('button',{name:'Queue message',exact:true}).click();
  await page.waitForFunction(()=>document.querySelectorAll('.draft-photo').length===0);
  assert(actions.some(a=>a.startsWith('Submit')&&a.includes('text: ""')&&a.includes('ChatImage')&&a.includes('width: 20')&&a.includes('iVBOR')),'photo was not normalized to PNG and sent');
  await page.locator('[data-action="remove-queued:20"]').click();
  await page.locator('#photo-input').setInputFiles([png,png,png,png,png]);
  await page.getByText('Attach up to four photos per message.',{exact:true}).waitFor();
  assert.equal(await page.locator('.draft-photo').count(),0);
  await page.locator('#photo-input').setInputFiles({name:'bad.png',mimeType:'image/png',buffer:Buffer.from('not an image')});
  await page.getByText(/This photo could not be decoded/).waitFor();
  await page.locator('#photo-input').setInputFiles(png);
  await page.waitForFunction(()=>document.querySelectorAll('.draft-photo').length===1 && !document.querySelector('#photo-input').disabled);
  await page.locator('#message').fill('reject');
  await page.getByRole('button',{name:'Queue message',exact:true}).click();
  await page.getByText(/Message not sent:/).waitFor();
  assert.equal(await page.locator('.draft-photo').count(),1,'rejection discarded photo');
  await page.getByRole('button',{name:'All agents',exact:true}).click();
  await page.locator('.home .chat .title').filter({hasText:'Polish the mobile experience'}).click();
  await page.waitForFunction(()=>document.querySelectorAll('.draft-photo').length===1);
  await page.getByRole('button',{name:'Remove photo 1',exact:true}).click();
  await page.waitForFunction(()=>document.querySelectorAll('.draft-photo').length===0);
  await page.locator('#message').fill('');
  // A decode finishing after navigation must not put the photo in the newly
  // opened chat, or allow a partial message to be sent before it finishes.
  await page.evaluate(()=>{
    window.savedCreateImageBitmap=window.createImageBitmap;
    window.createImageBitmap=async(...args)=>{
      await new Promise(resolve=>setTimeout(resolve,600));
      return window.savedCreateImageBitmap(...args);
    };
  });
  await page.locator('#photo-input').setInputFiles(png);
  await page.getByText('Preparing photos…',{exact:true}).waitFor();
  assert(await page.getByRole('button',{name:'Queue message',exact:true}).isDisabled());
  await page.getByRole('button',{name:'All agents',exact:true}).click();
  await page.locator('.home .chat .title').filter({hasText:'Review authentication changes'}).click();
  await page.locator('#message').waitFor();
  await page.waitForTimeout(700);
  assert.equal(await page.locator('.draft-photo').count(),0,'decode attached to the wrong chat');
  await page.getByRole('button',{name:'All agents',exact:true}).click();
  await page.locator('.home .chat .title').filter({hasText:'Polish the mobile experience'}).click();
  await page.waitForFunction(()=>document.querySelectorAll('.draft-photo').length===1);
  await page.getByRole('button',{name:'Remove photo 1',exact:true}).click();
  await page.evaluate(()=>{window.createImageBitmap=window.savedCreateImageBitmap;delete window.savedCreateImageBitmap;});

  await page.locator('#message').fill('Draft survives navigation');
  await page.getByRole('button',{name:'All agents',exact:true}).click();
  await page.locator('.home .chat .title').filter({hasText:'Polish the mobile experience'}).click();
  await page.waitForFunction(()=>document.querySelector('#message')?.value==='Draft survives navigation');
  await page.reload();
  await page.waitForFunction(()=>document.querySelector('#message')?.value==='Draft survives navigation');
  await page.locator('#message').fill('reject');
  await page.getByRole('button',{name:'Queue message',exact:true}).click();
  await page.locator('#chat-stage').getByText(/Message not sent:/).waitFor();
  assert.equal(await page.locator('#message').inputValue(),'reject');
  await page.locator('#message').fill('Keep working while steering');
  await page.getByRole('button',{name:'Steer now',exact:true}).last().click();
  await page.waitForFunction(()=>document.querySelector('#message')?.value==='');
  assert(actions.some(a=>a.startsWith('Submit')&&a.includes('steer: true')));
  await page.locator('#message').fill('delayed');
  await page.getByRole('button',{name:'Queue message',exact:true}).click();
  await page.getByRole('button',{name:'All agents',exact:true}).click();
  await page.waitForFunction(()=>sessionStorage.getItem('clackatoo.draft.1')===null);
  await page.reload();
  await page.locator('.home .chat .title').filter({hasText:'Polish the mobile experience'}).click();
  await page.locator('#message').waitFor();
  assert.equal(await page.locator('#message').inputValue(),'');
  await page.locator('#settings-trigger').click();
  await page.getByRole('dialog',{name:'Agent settings'}).waitFor();
  await page.screenshot({path:`${output}/phone-settings.png`});
  await page.getByLabel('Search all models').pressSequentially('Luna');
  await page.locator('.model-pick').filter({hasText:'6 Luna'}).waitFor();
  await page.waitForFunction(()=>document.querySelectorAll('.model-pick').length===1);
  assert.equal(await page.getByLabel('Search all models').inputValue(),'Luna');
  await page.getByRole('button',{name:'Add 6 Luna favorite',exact:true}).click();
  await page.getByLabel('Search all models').fill('');
  await page.locator('[data-model-provider="Favorites"]').click();
  await page.locator('.model-pick').filter({hasText:'6 Luna'}).click();
  await page.waitForFunction(()=>document.querySelector('.model-row.selected')?.textContent.includes('6 Luna'));
  await page.locator('[data-picker-tab="permissions"]').click();
  await page.getByRole('button',{name:'Ask',exact:true}).click();
  await page.waitForFunction(()=>document.querySelector('.setting-choice.selected')?.textContent==='Ask');
  await page.screenshot({path:`${output}/phone-permissions.png`});
  await page.keyboard.press('Escape');
  await page.getByRole('dialog').waitFor({state:'hidden'});
  await page.getByRole('button',{name:/context and usage limits/}).click();
  await page.getByRole('dialog',{name:'Context & usage'}).waitFor();
  await page.getByText('83% left',{exact:true}).waitFor();
  await page.screenshot({path:`${output}/phone-usage.png`});
  await page.keyboard.press('Escape');
  await page.getByRole('button',{name:'Harness capabilities',exact:true}).click();
  await page.getByRole('dialog',{name:'Harness capabilities'}).waitFor();
  await page.getByText('Off — no window granted',{exact:true}).waitFor();
  await noOverflow();
  await page.screenshot({path:`${output}/phone-capabilities.png`});
  await page.keyboard.press('Escape');
  await page.getByRole('dialog',{name:'Harness capabilities'}).waitFor({state:'hidden'});
  await page.getByRole('button',{name:'Stop activity',exact:true}).click();
  await page.getByText('Build mobile client · Stopping',{exact:true}).waitFor();
  assert(actions.includes('CancelActivity web-build'));
  await page.screenshot({path:`${output}/phone-activity.png`});
  await page.locator('.conversation').evaluate(el=>el.scrollTop=0);
  const anchor=page.locator('[data-turn="32"]');
  const topBefore=await anchor.evaluate(e=>{e.dataset.retained='yes';return e.getBoundingClientRect().top;});
  await page.waitForFunction(()=>document.querySelector('.turn')?.dataset.turn==='24');
  assert.equal(await anchor.getAttribute('data-retained'),'yes','prepending recreated an existing turn');
  assert(Math.abs((await anchor.boundingBox()).y-topBefore)<2,'history page moved the reading anchor');
  assert.equal(await page.locator('.turn').count(),16);
  assert(actions.includes('History 24 32'));
  await page.getByRole('button',{name:'↓ Latest',exact:true}).waitFor();
  await page.getByRole('button',{name:'↓ Latest',exact:true}).click();
  await page.waitForFunction(()=>{const e=document.querySelector('.conversation');return e.scrollTop+e.clientHeight>=e.scrollHeight-80;});
  await page.locator('.conversation').evaluate(el=>el.scrollTop=0);
  await page.waitForFunction(()=>document.querySelector('.turn')?.dataset.turn==='16');
  const scrollBeforeQueue=await page.locator('.conversation').evaluate(el=>el.scrollTop);
  await page.getByRole('button',{name:'Remove queued message',exact:true}).first().click();
  await page.getByText('Also check landscape mode',{exact:true}).waitFor({state:'hidden'});
  assert(Math.abs(await page.locator('.conversation').evaluate(el=>el.scrollTop)-scrollBeforeQueue)<2,'queue update moved the reading position');
  await page.locator('#message').fill('Kept across reconnect');
  const sentBefore=actions.filter(a=>a.startsWith('Submit')).length;
  sockets.at(-1).close({code:1001,reason:'test disconnect'});
  await page.getByText(/offline, retrying/).waitFor();
  assert(await page.locator('.send').isDisabled());
  await page.waitForFunction(()=>!document.querySelector('.chat-bar .meta')?.textContent.includes('offline'));
  assert.equal(await page.locator('#message').inputValue(),'Kept across reconnect');
  assert.equal(actions.filter(a=>a.startsWith('Submit')).length,sentBefore,'reconnect resubmitted a message');
  await page.locator('#message').fill('');
  assert.equal(await page.locator('.turn').first().getAttribute('data-turn'),'16','reconnect lost loaded history');
  for (const expected of [8,0]) {
    await page.locator('.conversation').evaluate(el=>el.scrollTop=0);
    await page.waitForFunction(first=>document.querySelector('.turn')?.dataset.turn===String(first),expected);
  }
  assert.equal(await page.locator('.turn').count(),40);
  await page.locator('.history-more').waitFor({state:'hidden'});
  await page.evaluate(()=>{
    Object.defineProperty(visualViewport,'height',{value:420,configurable:true});
    Object.defineProperty(visualViewport,'offsetTop',{value:18,configurable:true});
    visualViewport.dispatchEvent(new Event('resize'));
  });
  await tightComposer();
  assert(await page.locator('.chat-page').evaluate(e=>e.classList.contains('keyboard-open')));
  // Safari can pan the visual viewport to the layout viewport's bottom.
  // That does not mean the keyboard has gone away.
  await page.evaluate(()=>{
    Object.defineProperty(visualViewport,'offsetTop',{value:innerHeight-420,configurable:true});
    visualViewport.dispatchEvent(new Event('scroll'));
  });
  await page.waitForFunction(()=>Math.abs(document.querySelector('.composer').getBoundingClientRect().bottom-(visualViewport.offsetTop+visualViewport.height-6))<2);
  await tightComposer();
  // innerHeight can already reflect the keyboard while CSS 100dvh does not.
  await page.evaluate(()=>{
    const original=Object.getOwnPropertyDescriptor(window,'innerHeight');
    window.restoreInnerHeight=()=>Object.defineProperty(window,'innerHeight',original);
    Object.defineProperty(window,'innerHeight',{value:420,configurable:true});
    Object.defineProperty(visualViewport,'offsetTop',{value:0,configurable:true});
    visualViewport.dispatchEvent(new Event('resize'));
  });
  await page.waitForFunction(()=>document.querySelector('.composer').getBoundingClientRect().bottom<=420);
  await tightComposer();
  await page.evaluate(()=>{window.restoreInnerHeight();delete window.restoreInnerHeight;});
  await page.evaluate(()=>{delete visualViewport.height;delete visualViewport.offsetTop;visualViewport.dispatchEvent(new Event('resize'));});
  await tightComposer();
  assert(await page.locator('.chat-page').evaluate(e=>Math.abs(parseFloat(e.style.height)-visualViewport.height)<1),'stale keyboard height survived');
  // Focus/tap/restore may precede the new measurement, without a subsequent
  // resize event. These are event-order simulations, not an iOS keyboard.
  for (const trigger of ['focusin','pointerup','pageshow','visibilitychange']) {
    await page.waitForTimeout(1200); // Previous transition checks must be spent.
    await page.locator('.conversation').evaluate(el=>el.scrollTop=400);
    const reading=await page.locator('.conversation').evaluate(el=>el.scrollTop);
    await page.evaluate(async trigger=>{
      const target=trigger==='pageshow'?window:trigger==='visibilitychange'?document:document.querySelector('#message');
      target.dispatchEvent(new Event(trigger,{bubbles:true}));
      await new Promise(resolve=>setTimeout(resolve,200));
      Object.defineProperty(visualViewport,'height',{value:420,configurable:true});
      Object.defineProperty(visualViewport,'offsetTop',{value:18,configurable:true});
    },trigger);
    await tightComposer();
    assert(Math.abs(await page.locator('.conversation').evaluate(el=>el.scrollTop)-reading)<2,'keyboard moved the reading position');
    await page.evaluate(async()=>{
      document.querySelector('#message').dispatchEvent(new Event('focusout',{bubbles:true}));
      await new Promise(resolve=>setTimeout(resolve,200));
      delete visualViewport.height;
      delete visualViewport.offsetTop;
    });
    await tightComposer();
    assert(!await page.locator('.chat-page').evaluate(e=>e.classList.contains('keyboard-open')),'keyboard inset survived dismissal');
  }
  // Home Screen CSS vh can extend beyond the drawable web view. Never expand
  // into that clipped area, even if the difference matches safe-area padding.
  await page.evaluate(()=>{
    document.querySelector('.chat-page').style.paddingTop='62px';
  });
  await page.evaluate(()=>{
    Object.defineProperty(navigator,'standalone',{value:true,configurable:true});
    Object.defineProperty(visualViewport,'height',{value:innerHeight-62,configurable:true});
    visualViewport.dispatchEvent(new Event('resize'));
  });
  await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
  await tightComposer();
  await page.screenshot({path:`${output}/home-screen-composer.png`});
  await page.evaluate(()=>{
    Object.defineProperty(visualViewport,'height',{value:420,configurable:true});
    visualViewport.dispatchEvent(new Event('resize'));
  });
  await tightComposer();
  assert(await page.locator('.chat-page').evaluate(e=>e.classList.contains('keyboard-open')));
  await page.evaluate(()=>{
    Object.defineProperty(visualViewport,'height',{value:innerHeight-62,configurable:true});
    visualViewport.dispatchEvent(new Event('resize'));
  });
  await tightComposer();
  // Pinch zoom must still fit the visual viewport, even for a small reduction.
  await page.evaluate(()=>{
    Object.defineProperty(visualViewport,'scale',{value:1.1,configurable:true});
    visualViewport.dispatchEvent(new Event('resize'));
  });
  await tightComposer();
  await page.evaluate(()=>{
    document.querySelector('.chat-page').style.removeProperty('padding-top');
    delete navigator.standalone;delete visualViewport.height;delete visualViewport.scale;
    visualViewport.dispatchEvent(new Event('resize'));
  });
  await tightComposer();
  await page.setViewportSize({width:390,height:420});
  await page.locator('#message').focus();
  await page.waitForFunction(()=>{const r=document.querySelector('.send')?.getBoundingClientRect();return r && r.bottom<=420;});
  const send=await page.locator('.send').boundingBox();
  assert(send.y+send.height<=420,'composer outside keyboard-sized viewport');
  await page.screenshot({path:`${output}/phone-keyboard.png`});
  await page.setViewportSize({width:320,height:568});
  await noOverflow();
  await page.screenshot({path:`${output}/small-phone.png`});
  await page.getByRole('button',{name:'All agents',exact:true}).click();
  await page.locator('.home .chat .title').filter({hasText:'Review authentication changes'}).click();
  await page.getByRole('button',{name:'Allow once',exact:true}).waitFor();
  await page.screenshot({path:`${output}/phone-approval.png`});
  await page.getByRole('button',{name:'Allow once',exact:true}).click();
  await page.waitForFunction(()=>!document.querySelector('.question'));
  assert(actions.some(a=>a.startsWith('Answer')&&a.includes('allow')));
  await page.getByRole('button',{name:'All agents',exact:true}).click();
  await page.setViewportSize({width:1440,height:1000});
  await page.locator('.home .chat .title').filter({hasText:'Polish the mobile experience'}).waitFor();
  await page.screenshot({path:`${output}/desktop-home.png`});
  await page.locator('.home .chat .title').filter({hasText:'Polish the mobile experience'}).click();
  await page.screenshot({path:`${output}/desktop-chat.png`});
  await page.locator('.session-sidebar').getByText('Review authentication changes',{exact:true}).click();
  await page.locator('.chat-bar h1').filter({hasText:'Review authentication changes'}).waitFor();
  await page.locator('.session-sidebar').getByLabel('Search agents and workspaces').fill('mobile');
  await page.locator('.session-sidebar .chat .title').filter({hasText:'Polish the mobile experience'}).click();
  await page.locator('.chat-bar h1').filter({hasText:'Polish the mobile experience'}).waitFor();
  await page.setViewportSize({width:844,height:390});
  await noOverflow();
  await page.screenshot({path:`${output}/landscape-chat.png`});
  await page.getByRole('button',{name:'All agents',exact:true}).click();
  await page.getByLabel('Search agents and workspaces').fill('');
  await page.getByRole('button',{name:/Terminals/}).click();
  await page.getByText('Development shell',{exact:true}).click();
  await page.locator('.grid').getByText('Checking clackatoo-web',{exact:false}).waitFor();
  await page.setViewportSize({width:390,height:844});
  await page.getByRole('button',{name:'Fit',exact:true}).click();
  await page.getByRole('button',{name:'Fitted to the phone: tap to put back',exact:true}).waitFor();
  await page.screenshot({path:`${output}/phone-terminal.png`});
  await noOverflow();
  assert.deepEqual(errors,[]);
  console.log('PASS: dashboard filters/search; navigation and reload drafts; photo picker/PNG normalization/removal/image-only send/rejection/async navigation; native steering; global models/favorites/permissions/usage; paged history/anchors/reconnect; scroll/latest; delayed acknowledgments; 320px/390px/keyboard/landscape/desktop layout; approvals; image pixels; terminal Fit.');
  await context.close();
} finally { await browser.close(); server.close(); }
