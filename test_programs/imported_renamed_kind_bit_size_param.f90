! CHECK: 64
! CHECK: ok
! IR_CHECK: global @afs_mod_imported_renamed_kind_bit_size_param_m_block_size: i32 = 64
! REPRO_CHECK: run
module imported_renamed_kind_bit_size_param_kinds
  implicit none
  integer, parameter :: int32 = selected_int_kind(9)
  integer, parameter :: int64 = selected_int_kind(18)
end module

module imported_renamed_kind_bit_size_param_m
  use imported_renamed_kind_bit_size_param_kinds, only : bits_kind => int32, block_kind => int64
  implicit none
  integer(bits_kind), parameter :: block_size = bit_size(0_block_kind)
end module

program imported_renamed_kind_bit_size_param
  use imported_renamed_kind_bit_size_param_m, only : block_size
  implicit none
  if (block_size /= 64) error stop 1
  print *, block_size
  print *, 'ok'
end program
