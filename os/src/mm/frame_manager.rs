
use super::{PageTable, PhysPageNum, MemorySet, P2V_MAP, IDE_MANAGER};
use crate:: task::TASK_MANAGER;
use crate::config::PFF_T;
use crate::timer::get_time;
use alloc::vec::Vec;

#[derive(Debug)]
pub struct Queue<T> {
    data: Vec<T>,
}

impl <T> Queue<T> {
    pub fn new() -> Self {
        Queue{ data: Vec::new() }
    }

    pub fn push(&mut self, item: T) {
        self.data.push(item);
    }

    pub fn pop(&mut self) ->Option<T> {
        let l = self.data.len();
        if l > 0 {
            let v = self.data.remove(0);
            Some(v)
        } else {
            None
        }
    }

}

#[allow(unused)]
pub enum PRA {
    FIFO,
    Clock,
    ClockImproved,
    WorkingSet,
    PageFaultFrequency,
}

struct ClockQue {
    ppns: Vec<PhysPageNum>,
    ptr: usize,
}

impl ClockQue {
    fn new() -> Self {
        ClockQue{
            ppns: Vec::new(),
            ptr: 0,
        }
    }
    fn inc(&mut self) {
        if self.ptr == self.ppns.len() - 1 {
            self.ptr = 0;
        }
        else {
            self.ptr += 1;
        }
    }
    pub fn push(&mut self, ppn: PhysPageNum) {
        self.ppns.push(ppn);
    }

    pub fn pop(&mut self, page_table: &mut PageTable) -> Option<PhysPageNum> {
        loop {
            let ppn = self.ppns[self.ptr];
            let vpn = *(P2V_MAP.exclusive_access().get(&ppn).unwrap());
            let pte = page_table.find_pte(vpn).unwrap();
            if !pte.is_valid() {
                panic!("[kernel] PAGE FAULT: Pte not valid in PRA Clock pop.");
            }
            if !pte.accessed() {
                self.ppns.remove(self.ptr);
                if self.ptr == self.ppns.len() {
                    self.ptr = 0;
                }
                return Some(ppn);
            }
            pte.change_access();
            // println!("change pte access.");
            if pte.accessed() {
                panic!("[kernel] PAGE FAULT: Pte access did not change.");
            }
            self.inc();
        }
    }

    pub fn pop_improved(&mut self, page_table: &mut PageTable) -> Option<PhysPageNum> {
        loop {
            let ppn = self.ppns[self.ptr];
            let vpn = *(P2V_MAP.exclusive_access().get(&ppn).unwrap());
            let pte = page_table.find_pte(vpn).unwrap();
            if !pte.is_valid() {
                panic!("[kernel] PAGE FAULT: Pte not valid in PRA Clock pop.");
            }
            if !pte.accessed() && !pte.dirty() {
                self.ppns.remove(self.ptr);
                if self.ptr == self.ppns.len() {
                    self.ptr = 0;
                }
                return Some(ppn);
            }
            if pte.accessed() {
                pte.change_access();
                // println!("change pte access.");
                if pte.accessed() {
                    panic!("[kernel] PAGE FAULT: Pte access did not change.");
                }
            }
            else {
                pte.change_dirty();
                // println!("change pte dirty.");
                if pte.dirty() {
                    panic!("[kernel] PAGE FAULT: Pte dirty did not change.");
                }
            }
            self.inc();
        }
    }
}

pub struct LocalFrameManager {
    used_pra: PRA,
    fifo_que: Queue<PhysPageNum>,
    clock_que: ClockQue,
    pff_ppns: Vec<PhysPageNum>,
}

impl LocalFrameManager {
    pub fn new(pra: PRA) -> Self {
        LocalFrameManager{
            used_pra: pra,
            fifo_que: Queue::new(),
            clock_que: ClockQue::new(),
            pff_ppns: Vec::new(),
        }
    }
    pub fn get_next_frame(&mut self, page_table: &mut PageTable) -> Option<PhysPageNum> {
        match self.used_pra {
            PRA::FIFO => {
                self.fifo_que.pop()
            }
            PRA::Clock => {
                self.clock_que.pop(page_table)
            }
            PRA::ClockImproved => {
                self.clock_que.pop_improved(page_table)
            }
            _ => { None }
        }
    }
    pub fn insert_frame(&mut self, ppn: PhysPageNum) {
        match self.used_pra {
            PRA::FIFO => {
                self.fifo_que.push(ppn)
            }
            PRA::Clock => {
                self.clock_que.push(ppn)
            }
            PRA::ClockImproved => {
                self.clock_que.push(ppn)
            }
            PRA::PageFaultFrequency => {
                self.pff_ppns.push(ppn)
            }
            _ => {}
        }
    }
}

pub struct GlobalFrameManager {
    used_pra: PRA,
    t_last: usize,
}

impl GlobalFrameManager {
    pub fn new(pra: PRA) -> Self {
        GlobalFrameManager {
            used_pra: pra,
            t_last: 0,
        }
    }
    pub fn pff_work(&mut self, memory_set_: &mut MemorySet, token_: usize) {
        if self.t_last == 0 {
            self.t_last = get_time();
            return;
        }
        let t_current = get_time();
        let t_last = self.t_last;
        println!("[kernel] PAGE FAULT: t_current-t_last = {}", t_current-t_last);
        let task_manager = TASK_MANAGER.exclusive_access();
        if t_current-t_last > PFF_T {
            for i in 0..task_manager.ready_queue.len() {
                let process = task_manager.ready_queue[i].process.upgrade().unwrap();
                let mut pcb = process.inner_exclusive_access();
                let token = pcb.get_user_token();
                let memory_set = &mut pcb.memory_set;
                for j in (0..memory_set.frame_manager.pff_ppns.len()).rev() {
                    let ppn = memory_set.frame_manager.pff_ppns[j];
                    let data_old = ppn.get_bytes_array();
                    let mut p2v_map = P2V_MAP.exclusive_access();
                    let vpn = *(p2v_map.get(&ppn).unwrap());
                    if let Some(pte) = memory_set.page_table.translate(vpn) {
                        if pte.is_valid() && !pte.accessed() {
                            IDE_MANAGER.exclusive_access().swap_in(token, vpn, data_old);
                            for k in 0..memory_set.areas.len() {
                                if vpn >= memory_set.areas[k].vpn_range.get_start() && vpn < memory_set.areas[k].vpn_range.get_end() {
                                    memory_set.areas[k].unmap_one(&mut memory_set.page_table, vpn);
                                }
                            }
                            p2v_map.remove(&ppn);
                            memory_set.frame_manager.pff_ppns.remove(j);
                            println!("[kernel] PAGE FAULT: Swapping out ppn:{} frame.", ppn.0);
                        }
                    }
                }
            }
            for i in (0..memory_set_.frame_manager.pff_ppns.len()).rev() {
                let ppn = memory_set_.frame_manager.pff_ppns[i];
                let data = ppn.get_bytes_array();
                let mut p2v_map = P2V_MAP.exclusive_access();
                let vpn = *(p2v_map.get(&ppn).unwrap());
                if let Some(pte) = memory_set_.page_table.translate(vpn) {
                    // println!("pte.is_valid(): {} pte.accessed: {} pte.ppn: {} ppn: {}", pte.is_valid(), pte.accessed(), pte.ppn().0, ppn.0);
                    if pte.is_valid() && !pte.accessed() {
                        IDE_MANAGER.exclusive_access().swap_in(token_, vpn, data);
                        for k in 0..memory_set_.areas.len() {
                            if vpn >= memory_set_.areas[k].vpn_range.get_start() && vpn < memory_set_.areas[k].vpn_range.get_end() {
                                memory_set_.areas[k].unmap_one(&mut memory_set_.page_table, vpn);
                            }
                        }
                        p2v_map.remove(&ppn);
                        memory_set_.frame_manager.pff_ppns.remove(i);
                        println!("[kernel] PAGE FAULT: Swapping out ppn:{} frame.", ppn.0);
                    }
                }
            }
        }
        else {
            for i in 0..task_manager.ready_queue.len() {
                let process = task_manager.ready_queue[i].process.upgrade().unwrap();
                let mut pcb = process.inner_exclusive_access();
                let memory_set = &mut pcb.memory_set;
                for j in 0..memory_set.frame_manager.pff_ppns.len() {
                    let ppn = memory_set.frame_manager.pff_ppns[j];
                    let p2v_map = P2V_MAP.exclusive_access();
                    let vpn = *(p2v_map.get(&ppn).unwrap());
                    if let Some(pte) = memory_set.page_table.find_pte(vpn) {
                        if pte.is_valid() && pte.accessed() {
                            println!("global: changing pte access, ppn: {} pte.ppn: {}", ppn.0, pte.ppn().0);
                            pte.change_access();
                        }
                    }
                }
            }
            for i in 0..memory_set_.frame_manager.pff_ppns.len() {
                let ppn = memory_set_.frame_manager.pff_ppns[i];
                let p2v_map = P2V_MAP.exclusive_access();
                let vpn = *(p2v_map.get(&ppn).unwrap());
                if let Some(pte) = memory_set_.page_table.find_pte(vpn) {
                    if pte.is_valid() && pte.accessed() {
                        println!("global: changing pte access, ppn: {} pte.ppn: {}", ppn.0, pte.ppn().0);
                        pte.change_access();
                    }
                }
            }
        }
        self.t_last = t_current;
    }
}
