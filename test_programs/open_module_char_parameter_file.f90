! CHECK: done
! IR_CHECK: global_addr @afs_mod_open_module_file_names_filename
! IR_CHECK: call @afs_open
! FILE_MISSING: test_sorting.txt
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module open_module_file_names
  implicit none
  character(*), parameter :: filename = "test_sorting.txt"
contains
  subroutine write_and_delete
    integer :: lun

    open(newunit=lun, file=filename, access="sequential", action="write", &
         form="formatted", status="replace")
    write(lun, "(a)") "ok"
    close(lun, status="delete")
  end subroutine write_and_delete
end module open_module_file_names

program open_module_char_parameter_file
  use open_module_file_names, only: write_and_delete
  implicit none

  call write_and_delete
  write(*, "(a)") "done"
end program open_module_char_parameter_file
