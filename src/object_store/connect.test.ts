import test from 'ava'
import { ObjectStoreClient } from './connect.js'

test('ObjectStoreClient can be created with default options', (t) => {
  const client = new ObjectStoreClient()
  t.is(client.allowHttp, false)
  t.is(client.userAgent, null)
  t.is(client.proxyUrl, null)
  t.is(client.timeoutMs, null)
})

test('ObjectStoreClient respects custom options', (t) => {
  const client = new ObjectStoreClient({
    allowHttp: true,
    userAgent: 'custom-agent/1.0',
    proxyUrl: 'http://proxy.local:8080',
    timeoutMs: 5000,
  })
  t.is(client.allowHttp, true)
  t.is(client.userAgent, 'custom-agent/1.0')
  t.is(client.proxyUrl, 'http://proxy.local:8080')
  t.is(client.timeoutMs, 5000)
})
