use core::marker::PhantomData;

use bitfield::bitfield;

use crate::{
    acl::{AclPacket, BoundaryFlag, HostBroadcastFlag},
    crypto::{Addr, Check, Confirm, DHKey, IoCap, Nonce, PublicKey, SecretKey},
    l2cap::L2capPacket,
    Ble, Data,
};

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum IoCapability {
    DisplayOnly = 0,
    DisplayYesNo = 1,
    KeyboardOnly = 2,
    NoInputNoOutput = 3,
    KeyboardDisplay = 4,
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum OobDataFlag {
    NotPresent = 0,
    Present = 1,
}

bitfield! {
    pub struct AuthReq(u8);
    impl Debug;

    pub bonding_flags, set_bonding_flags: 1, 0;
    pub mitm, set_mitm: 2, 2;
    pub sc, set_sc: 3, 3;
    pub keypress, set_keypress: 4, 4;
    pub ct2, set_ct2: 5, 5;
    pub rfu, set_rfu: 7, 6;
}

const SM_PAIRING_REQUEST: u8 = 0x01;
const SM_PAIRING_RESPONSE: u8 = 0x02;
const SM_PAIRING_CONFIRM: u8 = 0x03;
const SM_PAIRING_RANDOM: u8 = 0x04;
const SM_PAIRING_PUBLIC_KEY: u8 = 0x0c;
const SM_PAIRING_DHKEY_CHECK: u8 = 0x0d;

pub struct SecurityManager<B> {
    skb: Option<SecretKey>,
    pkb: Option<PublicKey>,

    pka: Option<PublicKey>,

    confirm: Option<Confirm>,

    nb: Option<[u8; 16]>,

    dh_key: Option<DHKey>,

    eb: Option<Check>,

    pub local_address: Option<[u8; 6]>,
    pub peer_address: Option<[u8; 6]>,
    pub ltk: Option<u128>,

    phantom: PhantomData<B>,
}

pub trait BleWriter {
    fn write_bytes(&mut self, bytes: &[u8]);
}

impl<'a> BleWriter for Ble<'a> {
    fn write_bytes(&mut self, bytes: &[u8]) {
        self.write_bytes(bytes);
    }
}

impl<B> Default for SecurityManager<B> {
    fn default() -> Self {
        Self {
            skb: None,
            pkb: None,
            pka: None,
            confirm: None,
            nb: None,
            dh_key: None,
            eb: None,
            local_address: None,
            peer_address: None,
            ltk: None,
            phantom: PhantomData::default(),
        }
    }
}

#[cfg(feature = "async")]
pub struct AsyncSecurityManager<B> {
    skb: Option<SecretKey>,
    pkb: Option<PublicKey>,

    pka: Option<PublicKey>,

    confirm: Option<Confirm>,

    nb: Option<[u8; 16]>,

    dh_key: Option<DHKey>,

    eb: Option<Check>,

    pub local_address: Option<[u8; 6]>,
    pub peer_address: Option<[u8; 6]>,
    pub ltk: Option<u128>,

    phantom: PhantomData<B>,
}

#[cfg(feature = "async")]
pub trait AsyncBleWriter {
    async fn write_bytes(&mut self, bytes: &[u8]);
}

#[cfg(feature = "async")]
impl<T> AsyncBleWriter for crate::asynch::Ble<T>
where
    T: embedded_io_async::Read + embedded_io_async::Write,
{
    async fn write_bytes(&mut self, bytes: &[u8]) {
        self.write_bytes(bytes).await
    }
}

#[cfg(feature = "async")]
impl<B> Default for AsyncSecurityManager<B> {
    fn default() -> Self {
        Self {
            skb: None,
            pkb: None,
            pka: None,
            confirm: None,
            nb: None,
            dh_key: None,
            eb: None,
            local_address: None,
            peer_address: None,
            ltk: None,
            phantom: PhantomData::default(),
        }
    }
}

bleps_dedup::dedup! {
impl<B> SYNC SecurityManager<B> where B: BleWriter
impl<B> ASYNC AsyncSecurityManager<B> where B: AsyncBleWriter
 {
    pub(crate) async fn handle(&mut self, ble: &mut B, src_handle: u16, payload: crate::Data) {
        log::info!("SM packet {:02x?}", payload.as_slice());

        let data = &payload.as_slice()[1..];
        let command = payload.as_slice()[0];

        match command {
            SM_PAIRING_REQUEST => {
                self.handle_pairing_request(ble, src_handle, data).await;
            }
            SM_PAIRING_PUBLIC_KEY => {
                self.handle_pairing_public_key(ble, src_handle, data).await;
            }
            SM_PAIRING_RANDOM => {
                self.handle_pairing_random(ble, src_handle, data).await;
            }
            SM_PAIRING_DHKEY_CHECK => {
                self.handle_pairing_dhkey_check(ble, src_handle, data).await;
            }
            // handle FAILURE
            _ => {
                log::error!("Unknown SM command {}", command);
            }
        }
    }

    async fn handle_pairing_request(&mut self, ble: &mut B, src_handle: u16, _data: &[u8]) {
        log::info!("got pairing request");

        let mut auth_req = AuthReq(0);
        auth_req.set_bonding_flags(1);
        auth_req.set_mitm(1);
        auth_req.set_sc(1);
        auth_req.set_keypress(0);
        auth_req.set_ct2(1);

        let mut data = Data::new(&[SM_PAIRING_RESPONSE]);
        data.append_value(IoCapability::DisplayYesNo as u8);
        data.append_value(OobDataFlag::NotPresent as u8);
        data.append_value(auth_req.0);
        data.append_value(0x10u8);
        data.append_value(0u8); // 3
        data.append_value(0u8); // 3

        self.write_sm(ble, src_handle, data).await;
    }

    async fn handle_pairing_public_key(&mut self, ble: &mut B, src_handle: u16, pka: &[u8]) {
        log::info!("got public key");

        log::info!("key len = {} {:02x?}", pka.len(), pka);
        let pka = PublicKey::from_bytes(pka);

        // Send the local public key before validating the remote key to allow
        // parallel computation of DHKey. No security risk in doing so.

        let mut data = Data::new(&[SM_PAIRING_PUBLIC_KEY]);

        let skb = SecretKey::new();
        let pkb = skb.public_key();

        let mut x = [0u8; 32];
        let mut y = [0u8; 32];
        x.copy_from_slice(pkb.x.as_be_bytes());
        y.copy_from_slice(pkb.y.as_be_bytes());
        x.reverse();
        y.reverse();

        data.append(&x);
        data.append(&y);
        self.write_sm(ble, src_handle, data).await;

        let dh_key = skb.dh_key(pka).unwrap();

        // SUBTLE: The order of these send/recv ops is important. See last
        // paragraph of Section 2.3.5.6.2.
        let nb = Nonce::new();
        let cb = nb.f4(pkb.x(), pka.x(), 0);

        let mut data = Data::new(&[SM_PAIRING_CONFIRM]);
        let confirm_value = cb.0.to_le_bytes();
        data.append(&confirm_value);
        self.write_sm(ble, src_handle, data).await;

        self.pka = Some(pka);
        self.pkb = Some(pkb);
        self.skb = Some(skb);
        self.confirm = Some(cb);
        self.nb = Some(nb.0.to_le_bytes().try_into().unwrap());
        self.dh_key = Some(dh_key);
    }

    async fn handle_pairing_random(&mut self, ble: &mut B, src_handle: u16, random: &[u8]) {
        log::info!("got pairing random {:02x?}", random);

        let mut data = Data::new(&[SM_PAIRING_RANDOM]);
        let mut tmp_random = [0u8; 16];
        tmp_random.copy_from_slice(self.nb.as_ref().unwrap());
        data.append(&tmp_random);
        self.write_sm(ble, src_handle, data).await;

        let na = Nonce(u128::from_le_bytes(random.try_into().unwrap()));
        let nb = Nonce(u128::from_le_bytes(self.nb.unwrap()));
        let vb = na.g2(
            self.pka.as_ref().unwrap().x(),
            self.pkb.as_ref().unwrap().x(),
            &nb,
        );

        // should display the code and get confirmation from user (pin ok or not) - if not okay send a pairing-failed
        // assume it's correct or the user will cancel on central
        log::info!("Display code is {}", vb.0);

        let local_addr = self.local_address.unwrap();
        let peer_addr = self.peer_address.unwrap();

        // Authentication stage 2 and long term key calculation
        // ([Vol 3] Part H, Section 2.3.5.6.5 and C.2.2.4).

        let a = Addr::from_le_bytes(false, peer_addr);
        let b = Addr::from_le_bytes(false, local_addr);
        let ra = 0;
        log::info!("a = {:02x?}", a.0);
        log::info!("b = {:02x?}", b.0);

        let mut auth_req = AuthReq(0);
        auth_req.set_bonding_flags(1);
        auth_req.set_mitm(1);
        auth_req.set_sc(1);
        auth_req.set_keypress(0);
        auth_req.set_ct2(1);
        let io_cap = IoCapability::DisplayYesNo as u8;
        let iob = IoCap::new(auth_req.0, false, io_cap);
        let dh_key = self.dh_key.as_ref().unwrap();

        let (mac_key, ltk) = dh_key.f5(na, nb, a, b);
        let eb = mac_key.f6(nb, na, ra, iob, b, a);

        self.ltk = Some(ltk.0);
        self.eb = Some(eb);
    }

    async fn handle_pairing_dhkey_check(&mut self, ble: &mut B, src_handle: u16, ea: &[u8]) {
        log::info!("got dhkey_check {:02x?}", ea);

        // TODO ... check the DHKEY
        // if ea != mac_key.f6(na, nb, rb, ioa, a, b) {
        //    fail(Reason::DhKeyCheckFailed)
        // }

        let mut data = Data::new(&[SM_PAIRING_DHKEY_CHECK]);
        data.append(&self.eb.as_ref().unwrap().0.to_le_bytes());
        self.write_sm(ble, src_handle, data).await;
    }

    async fn write_sm(&self, ble: &mut B, handle: u16, data: Data) {
        log::debug!("data {:x?}", data.as_slice());

        let res = L2capPacket::encode_sm(data);
        log::info!("encoded_l2cap {:x?}", res.as_slice());

        let res = AclPacket::encode(
            handle,
            BoundaryFlag::FirstAutoFlushable,
            HostBroadcastFlag::NoBroadcast,
            res,
        );

        log::info!("writing {:02x?}", res.as_slice());
        ble.write_bytes(res.as_slice()).await;
    }

}
}
