use std::sync::Arc;

use crate::dpb::{DecodedPicture, Dpb};
use crate::error::DecodeError;
use crate::pps::Pps;
use crate::slice::{SliceHeader, SliceType};
use crate::sps::Sps;

pub(crate) struct ReferenceLists {
    pub p: Vec<Arc<DecodedPicture>>,
    pub l0: Vec<Arc<DecodedPicture>>,
    pub l1: Vec<Arc<DecodedPicture>>,
}

fn pad(list: &mut Vec<Arc<DecodedPicture>>, count: u32) -> Result<(), DecodeError> {
    if count == 0 || count > 32 {
        return Err(DecodeError::InvalidSyntax("invalid active reference count"));
    }
    let last = list
        .last()
        .cloned()
        .ok_or(DecodeError::InvalidSyntax("missing reference picture"))?;
    while list.len() < count as usize {
        list.push(Arc::clone(&last));
    }
    list.truncate(count as usize);
    Ok(())
}

pub(crate) fn reference_lists(
    dpb: &Dpb,
    sps: &Sps,
    header: &SliceHeader,
    current_poc: i32,
) -> Result<ReferenceLists, DecodeError> {
    let field = header.field_pic_flag;
    let bottom = header.bottom_field_flag;
    let shift = sps
        .log2_max_frame_num_minus4
        .checked_add(4)
        .filter(|shift| *shift < 32)
        .ok_or(DecodeError::InvalidSyntax("invalid frame number width"))?;
    let max_frame_num = 1_u32
        .checked_shl(shift)
        .ok_or(DecodeError::InvalidSyntax("frame number width overflow"))?;
    let max_pic_num = if field {
        max_frame_num
            .checked_mul(2)
            .ok_or(DecodeError::InvalidSyntax("field picture number overflow"))?
    } else {
        max_frame_num
    };
    let curr_pic_num = if field {
        header
            .frame_num
            .checked_mul(2)
            .and_then(|number| number.checked_add(1))
            .ok_or(DecodeError::InvalidSyntax("field picture number overflow"))?
    } else {
        header.frame_num
    };

    let mut lists = ReferenceLists {
        p: Vec::new(),
        l0: Vec::new(),
        l1: Vec::new(),
    };
    match header.slice_type {
        SliceType::P => {
            lists.p = dpb.short_term_ref_list(field, bottom);
            pad(&mut lists.p, header.num_ref_idx_l0_active)?;
            Dpb::apply_ref_list_modification(
                &mut lists.p,
                &header.ref_list_mod_l0,
                curr_pic_num,
                max_pic_num,
                field,
                bottom,
            )?;
        }
        SliceType::B => {
            lists.l0 = dpb.ref_list_l0_b(current_poc, field, bottom);
            lists.l1 = dpb.ref_list_l1_b(current_poc, field, bottom);
            pad(&mut lists.l0, header.num_ref_idx_l0_active)?;
            pad(&mut lists.l1, header.num_ref_idx_l1_active)?;
            Dpb::apply_ref_list_modification(
                &mut lists.l0,
                &header.ref_list_mod_l0,
                curr_pic_num,
                max_pic_num,
                field,
                bottom,
            )?;
            Dpb::apply_ref_list_modification(
                &mut lists.l1,
                &header.ref_list_mod_l1,
                curr_pic_num,
                max_pic_num,
                field,
                bottom,
            )?;
        }
        SliceType::I => {}
        _ => return Err(DecodeError::Unsupported("unsupported slice type")),
    }
    Ok(lists)
}

pub(crate) fn weights(
    pps: &Pps,
    header: &SliceHeader,
    current_poc: i32,
    lists: &ReferenceLists,
) -> Result<(u8, Vec<Vec<i32>>), DecodeError> {
    let mode = if header.slice_type == SliceType::P && pps.weighted_pred_flag
        || header.slice_type == SliceType::B && pps.weighted_bipred_idc == 1
    {
        1
    } else if header.slice_type == SliceType::B && pps.weighted_bipred_idc == 2 {
        2
    } else {
        0
    };
    if mode == 1 && header.weight_table.is_none() {
        return Err(DecodeError::InvalidSyntax(
            "missing explicit prediction weights",
        ));
    }
    if mode != 2 {
        return Ok((mode, Vec::new()));
    }
    let implicit = lists
        .l0
        .iter()
        .map(|l0| {
            lists
                .l1
                .iter()
                .map(|l1| {
                    let td = (i64::from(l1.pic_order_cnt) - i64::from(l0.pic_order_cnt))
                        .clamp(-128, 127) as i32;
                    if td == 0 {
                        return 32;
                    }
                    let tb = (i64::from(current_poc) - i64::from(l0.pic_order_cnt)).clamp(-128, 127)
                        as i32;
                    let tx = (16384 + td.abs() / 2) / td;
                    let w1 = (tb * tx + 32) >> 8;
                    if (-64..=128).contains(&w1) {
                        64 - w1
                    } else {
                        32
                    }
                })
                .collect()
        })
        .collect();
    Ok((mode, implicit))
}
