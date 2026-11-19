import { readFileSync } from 'node:fs';
import { test, before, after } from 'node:test';
import { initializeTestEnvironment, assertSucceeds, assertFails } from '@firebase/rules-unit-testing';
import { doc, setDoc, getDoc, deleteDoc, serverTimestamp, collection, getDocs } from 'firebase/firestore';

let env;
before(async () => {
  env = await initializeTestEnvironment({projectId:'demo-pebble-desktop', firestore:{host:'127.0.0.1',port:8187,rules:readFileSync(process.env.PEBBLE_RULES_FILE || new URL('./firestore.rules',import.meta.url),'utf8')}});
  await env.withSecurityRulesDisabled(async ctx => {
    await setDoc(doc(ctx.firestore(),'users/alice'),{existing:true});
    await setDoc(doc(ctx.firestore(),'users/bob'),{existing:true});
  });
});
after(async () => { await env?.cleanup(); });
const value = () => ({ computerName:'Test Mac',macValidationData:'synthetic-test-key',platform:'macOS',formatVersion:1,createdAt:serverTimestamp() });
const path = 'chat_activation_keys/alice/keys/mac-1';

test('owner can save, enumerate, update, and remove their key', async () => {
  const db=env.authenticatedContext('alice').firestore();
  await assertSucceeds(setDoc(doc(db,path),value()));
  await assertSucceeds(getDoc(doc(db,path)));
  await assertSucceeds(getDocs(collection(db,'chat_activation_keys/alice/keys')));
  await assertSucceeds(setDoc(doc(db,path),{...value(),computerName:'Renamed Mac'}));
  await assertSucceeds(deleteDoc(doc(db,path)));
});
test('other users and signed-out clients cannot read or mutate keys', async () => {
  for(const context of [env.authenticatedContext('bob'),env.unauthenticatedContext()]) {
    const db=context.firestore();
    await assertFails(getDoc(doc(db,path)));
    await assertFails(getDocs(collection(db,'chat_activation_keys/alice/keys')));
    await assertFails(setDoc(doc(db,path),value()));
    await assertFails(deleteDoc(doc(db,path)));
  }
});
test('new Firebase identity cannot save before a Pebble account exists',async()=>{
  const db=env.authenticatedContext('new-user').firestore();
  await assertFails(setDoc(doc(db,'chat_activation_keys/new-user/keys/mac'),value()));
});
test('rejects extra fields, fake timestamps, and malformed keys', async()=>{
  const db=env.authenticatedContext('alice').firestore();
  for(const invalid of [{extra:true},{createdAt:new Date(0)},{macValidationData:''},{macValidationData:'x'.repeat(32769)},{computerName:''},{platform:'other'},{formatVersion:2}]){
    await assertFails(setDoc(doc(db,path),{...value(),...invalid}));
  }
});
