<template>
  <div class="profile-page">
    <div class="cover-section">
      <div class="cover-bg"></div>
      <div class="profile-info">
        <div class="avatar-wrap">
          <div class="big-avatar">{{ (userStore.user?.displayName || 'U').charAt(0).toUpperCase() }}</div>
        </div>
        <div class="user-meta">
          <h3 class="user-name">{{ userStore.user?.displayName || userStore.user?.username }}
            <a-tag :color="roleColor" size="small">{{ roleLabel }}</a-tag>
            <a-tag v-if="twofaStatus.enabled" color="green" size="small">2FA</a-tag>
          </h3>
          <p class="user-email">@{{ userStore.user?.username }}</p>
        </div>
      </div>
    </div>

    <div class="pill-tabs">
      <div v-for="t in tabs" :key="t.key" class="pill-tab" :class="{ active: activeTab === t.key }" @click="activeTab = t.key">
        <component :is="t.icon" /><span>{{ t.label }}</span>
      </div>
    </div>

    <!-- 个人资料 -->
    <div v-show="activeTab === 'profile'" class="tab-content">
      <a-card>
        <a-form :model="profileForm" layout="vertical" style="max-width: 640px">
          <a-row :gutter="16">
            <a-col :span="12"><a-form-item label="显示名称"><a-input v-model="profileForm.displayName" /></a-form-item></a-col>
            <a-col :span="12"><a-form-item label="用户名"><a-input :model-value="userStore.user?.username" disabled /></a-form-item></a-col>
          </a-row>
          <a-row :gutter="16">
            <a-col :span="12"><a-form-item label="邮箱"><a-input v-model="profileForm.email" placeholder="user@example.com" /></a-form-item></a-col>
            <a-col :span="12"><a-form-item label="关联 Agent"><a-input v-model="profileForm.agentId" disabled /></a-form-item></a-col>
          </a-row>
          <a-button type="primary" :loading="savingProfile" @click="saveProfile">保存修改</a-button>
        </a-form>
      </a-card>
    </div>

    <!-- 安全 -->
    <div v-show="activeTab === 'security'" class="tab-content">
      <a-card title="修改密码" class="mb-16">
        <a-form layout="vertical" style="max-width: 640px">
          <a-form-item label="当前密码"><a-input-password v-model="pwForm.oldPassword" /></a-form-item>
          <a-row :gutter="16">
            <a-col :span="12"><a-form-item label="新密码"><a-input-password v-model="pwForm.newPassword" /></a-form-item></a-col>
            <a-col :span="12"><a-form-item label="确认密码"><a-input-password v-model="pwForm.confirmPassword" /></a-form-item></a-col>
          </a-row>
          <a-button type="primary" :loading="savingPw" @click="changePassword">修改密码</a-button>
        </a-form>
      </a-card>

      <a-card title="二步验证">
        <div class="twofa-status-row">
          <div>
            <div class="twofa-title">{{ twofaStatus.enabled ? '已启用' : '未启用' }}</div>
            <div class="twofa-desc">{{ twofaStatus.enabled ? '方式: ' + (twofaStatus.method === 'totp' ? '身份验证器' : '邮件') : '启用后登录需额外验证码' }}</div>
          </div>
          <a-tag :color="twofaStatus.enabled ? 'green' : 'gray'">{{ twofaStatus.enabled ? '已启用' : '未启用' }}</a-tag>
        </div>
        <div v-if="!twofaStatus.enabled" class="mt-16">
          <a-radio-group v-model="twofaMethod" type="button">
            <a-radio value="totp">身份验证器</a-radio>
            <a-radio value="email">邮件验证码</a-radio>
          </a-radio-group>
          <div class="mt-8">
            <a-button type="primary" :loading="settingUp2fa" @click="start2faSetup">启用</a-button>
          </div>
        </div>
        <div v-if="totpSetup.secret" class="mt-16">
          <p>扫码添加到身份验证器：</p>
          <img :src="totpSetup.qrUrl" v-if="totpSetup.qrUrl" style="width:200px;border-radius:8px" />
          <a-input :model-value="totpSetup.secret" readonly class="mt-8" />
          <a-input v-model="totpCode" placeholder="6 位验证码" :max-length="6" style="width:200px;margin-top:8px;text-align:center;font-size:20px" />
          <a-button type="primary" :loading="verifying2fa" @click="verify2faCode" style="margin-left:8px">确认</a-button>
        </div>
        <div v-if="email2faSent" class="mt-16">
          <p>验证码已发送到 {{ profileForm.email }}</p>
          <a-input v-model="emailCode" placeholder="6 位验证码" :max-length="6" style="width:200px;text-align:center;font-size:20px" />
          <a-button type="primary" :loading="verifying2fa" @click="verifyEmail2fa" style="margin-left:8px">确认</a-button>
        </div>
        <a-button v-if="twofaStatus.enabled" status="danger" class="mt-16" @click="disable2fa">关闭二步验证</a-button>
      </a-card>
    </div>

    <!-- 偏好 -->
    <div v-show="activeTab === 'preference'" class="tab-content">
      <a-row :gutter="16">
        <a-col :xs="24" :lg="12">
          <a-card title="外观" class="mb-16">
            <div class="theme-selector">
              <div v-for="t in themeOptions" :key="t.value" class="theme-card" :class="{ active: appStore.themeMode === t.value }" @click="appStore.setThemeMode(t.value)">
                <div class="theme-preview" :class="t.value"><div class="tp-bar"></div><div class="tp-bar short"></div></div>
                <div class="theme-label"><component :is="t.icon" /> {{ t.label }} <icon-check v-if="appStore.themeMode === t.value" style="color:var(--brand-success)" /></div>
              </div>
            </div>
          </a-card>
        </a-col>
        <a-col :xs="24" :lg="12">
          <a-card title="其他">
            <a-form layout="vertical">
              <a-form-item label="语言">
                <a-radio-group v-model="appStore.locale" type="button" @change="(v:any) => appStore.setLocale(v)">
                  <a-radio value="zh-CN">中文</a-radio><a-radio value="en-US">English</a-radio>
                </a-radio-group>
              </a-form-item>
              <a-form-item label="侧栏默认折叠"><a-switch v-model="appStore.menuCollapse" /></a-form-item>
            </a-form>
          </a-card>
        </a-col>
      </a-row>
    </div>

    <!-- API Token -->
    <div v-show="activeTab === 'token'" class="tab-content">
      <a-card>
        <a-alert type="info" class="mb-16">API Token 用于程序化调用网关 RPC</a-alert>
        <a-input-password :model-value="rpc.getToken()" readonly />
        <a-button type="primary" class="mt-8" @click="copyText(rpc.getToken())">复制 Token</a-button>
      </a-card>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, reactive, computed, onMounted, markRaw } from 'vue';
import { Message } from '@arco-design/web-vue';
import { rpc } from '@/api/rpc';
import { useUserStore } from '@/stores/user';
import { useAppStore } from '@/stores/app';
import { IconUser, IconSafe, IconSettings, IconLock, IconCheck, IconSunFill, IconMoonFill, IconComputer } from '@arco-design/web-vue/es/icon';

const userStore = useUserStore();
const appStore = useAppStore();

const tabs = [
  { key: 'profile', label: '资料', icon: markRaw(IconUser) },
  { key: 'security', label: '安全', icon: markRaw(IconSafe) },
  { key: 'preference', label: '偏好', icon: markRaw(IconSettings) },
  { key: 'token', label: 'API Token', icon: markRaw(IconLock) },
];
const activeTab = ref('profile');
const savingProfile = ref(false);
const savingPw = ref(false);
const profileForm = reactive({ displayName: userStore.user?.displayName || '', email: userStore.user?.email || '', agentId: userStore.user?.agentId || 'main' });
const pwForm = reactive({ oldPassword: '', newPassword: '', confirmPassword: '' });

const roleLabel = computed(() => ({ admin:'管理员', manager:'经理', supervisor:'主管', operator:'操作员', viewer:'访客' } as any)[userStore.role] || userStore.role);
const roleColor = computed(() => ({ admin:'red', manager:'purple', supervisor:'arcoblue', operator:'green', viewer:'gray' } as any)[userStore.role] || 'gray');

const themeOptions = [
  { value: 'light' as const, label: '浅色', icon: markRaw(IconSunFill) },
  { value: 'dark' as const, label: '深色', icon: markRaw(IconMoonFill) },
  { value: 'system' as const, label: '跟随系统', icon: markRaw(IconComputer) },
];

async function saveProfile() {
  savingProfile.value = true;
  try { await rpc.call('users.updateProfile', { displayName: profileForm.displayName, email: profileForm.email }); Message.success('已保存'); await userStore.refresh(); }
  catch (e: any) { Message.error(e.message); } finally { savingProfile.value = false; }
}
async function changePassword() {
  if (!pwForm.oldPassword || !pwForm.newPassword) { Message.warning('请填写密码'); return; }
  if (pwForm.newPassword !== pwForm.confirmPassword) { Message.warning('两次密码不一致'); return; }
  savingPw.value = true;
  try { await rpc.call('users.changePassword', { oldPassword: pwForm.oldPassword, newPassword: pwForm.newPassword }); Message.success('已修改'); pwForm.oldPassword=''; pwForm.newPassword=''; pwForm.confirmPassword=''; }
  catch (e: any) { Message.error(e.message); } finally { savingPw.value = false; }
}

// 2FA
const twofaStatus = reactive({ enabled: false, method: '' });
const twofaMethod = ref<'totp'|'email'>('totp');
const settingUp2fa = ref(false);
const verifying2fa = ref(false);
const totpSetup = reactive({ secret: '', qrUrl: '', uri: '' });
const totpCode = ref('');
const email2faSent = ref(false);
const emailCode = ref('');

async function load2faStatus() { try { const r = await rpc.call<any>('auth.2fa.status'); twofaStatus.enabled = r.enabled||false; twofaStatus.method = r.method||''; } catch {} }
async function start2faSetup() {
  settingUp2fa.value = true; totpSetup.secret=''; totpSetup.qrUrl=''; email2faSent.value=false;
  try {
    if (twofaMethod.value === 'totp') { const r = await rpc.call<any>('auth.2fa.setup', {method:'totp'}); totpSetup.secret=r.secret||''; totpSetup.uri=r.otpauthUri||r.uri||''; if(totpSetup.uri) totpSetup.qrUrl=`https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=${encodeURIComponent(totpSetup.uri)}`; }
    else { await rpc.call('auth.2fa.send_email_code'); email2faSent.value=true; Message.success('已发送'); }
  } catch(e:any){Message.error(e.message)} finally { settingUp2fa.value=false; }
}
async function verify2faCode() { if(!totpCode.value||totpCode.value.length!==6){Message.warning('请输入6位码');return;} verifying2fa.value=true; try{await rpc.call('auth.2fa.verify',{code:totpCode.value,method:'totp'});Message.success('已启用');totpSetup.secret='';totpCode.value='';await load2faStatus();}catch(e:any){Message.error(e.message)}finally{verifying2fa.value=false;} }
async function verifyEmail2fa() { if(!emailCode.value||emailCode.value.length!==6){Message.warning('请输入6位码');return;} verifying2fa.value=true; try{await rpc.call('auth.2fa.verify',{code:emailCode.value,method:'email'});Message.success('已启用');email2faSent.value=false;emailCode.value='';await load2faStatus();}catch(e:any){Message.error(e.message)}finally{verifying2fa.value=false;} }
async function disable2fa() { try{await rpc.call('auth.2fa.disable');Message.success('已关闭');await load2faStatus();}catch(e:any){Message.error(e.message)} }
function copyText(t:string){navigator.clipboard.writeText(t).then(()=>Message.success('已复制'))}

onMounted(() => { load2faStatus(); });
</script>

<style lang="less" scoped>
.profile-page { max-width: 1200px; margin: 0 auto; }
.cover-section { border-radius: var(--card-radius) var(--card-radius) 0 0; overflow: hidden; }
.cover-bg { height: 160px; background: linear-gradient(135deg, #8c57ff, #7340e0 40%, #16b1ff); position: relative;
  &::after { content:''; position:absolute; inset:0; background: radial-gradient(circle at 30% 50%, rgba(255,255,255,0.15), transparent 60%); } }
.profile-info { display: flex; align-items: flex-end; gap: 20px; padding: 0 32px 16px; margin-top: -50px; position: relative; z-index: 2; }
.big-avatar { width: 96px; height: 96px; border-radius: 50%; background: var(--brand-primary); color: #fff; display: flex; align-items: center; justify-content: center; font-size: 38px; font-weight: 700; border: 4px solid var(--color-bg-1); box-shadow: 0 4px 16px rgba(0,0,0,0.15); }
.user-name { margin: 0; font-size: 22px; font-weight: 700; display: flex; align-items: center; gap: 8px; }
.user-email { margin: 4px 0 0; font-size: 14px; color: var(--color-text-3); }
.pill-tabs { display: flex; gap: 8px; padding: 0 32px; border-bottom: 1px solid var(--color-border-1); margin-bottom: 24px; }
.pill-tab { display: flex; align-items: center; gap: 8px; padding: 12px 20px; font-size: 14px; font-weight: 500; color: var(--color-text-3); cursor: pointer; border-bottom: 3px solid transparent; transition: all .2s;
  &:hover { color: var(--brand-primary); }
  &.active { color: var(--brand-primary); border-bottom-color: var(--brand-primary); } }
.tab-content { padding: 0 32px 32px; }
.twofa-status-row { display: flex; justify-content: space-between; align-items: center; padding: 12px 0; }
.twofa-title { font-size: 15px; font-weight: 600; }
.twofa-desc { font-size: 13px; color: var(--color-text-3); margin-top: 4px; }
.theme-selector { display: flex; gap: 16px; }
.theme-card { width: 120px; border: 2px solid var(--color-border-2); border-radius: 8px; padding: 12px; cursor: pointer; text-align: center; transition: all .2s;
  &:hover { border-color: var(--brand-primary); }
  &.active { border-color: var(--brand-primary); background: rgba(140,87,255,0.05); } }
.theme-preview { height: 56px; border-radius: 6px; margin-bottom: 8px; overflow: hidden;
  &.light { background: #f8f7fa; }
  &.dark { background: #28243d; }
  &.system { background: linear-gradient(135deg, #f8f7fa 50%, #28243d 50%); }
  .tp-bar { height: 8px; background: var(--brand-primary); margin: 6px 8px; border-radius: 2px; opacity: 0.6;
    &.short { width: 60%; opacity: 0.3; } } }
.theme-label { font-size: 13px; display: flex; align-items: center; justify-content: center; gap: 4px; }
.mt-8 { margin-top: 8px; } .mt-16 { margin-top: 16px; } .mb-16 { margin-bottom: 16px; }
</style>
